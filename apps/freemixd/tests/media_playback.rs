//! Process-level IN-003 on the simulated runtime: media-file inputs play
//! from the engine's synthetic source (ten seconds at the program rate with
//! an English and a Spanish track), paced by the daemon's media clock in
//! wall time. Transport and settings commands are journaled, settings
//! persist across restart, the snapshot's `media_state` and `media` runtime
//! events report the transport, and an end trigger of the media input the
//! playlist airs (an audio-only WAV sting, IN-005) advances the playlist.

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
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, MediaOperation, MediaSeekTarget, MediaStateMessage,
    MediaStatus, PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistRunState,
    PlaylistStart, PlaylistTransitionKind, Role, SnapshotMessage, WireInputId, WireMessage,
    decode_line, encode_line,
};
#[cfg(unix)]
use fm_protocol::{MediaEndAction, MediaEvent, MediaTransportState, RuntimeLifecycleEvent};
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
            "freemixd-media-{}-{sequence}-{name}",
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

/// Input 1 a solid generator on Program, input 2 the media input of
/// `asset_uri` on Preview (with `settings`), input 3 another solid
/// generator; 25 fps.
fn save_project(path: &Path, asset_uri: &str, settings: MediaPlaybackSettings) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(44).unwrap()),
        "Media playback",
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
                asset_uri: asset_uri.into(),
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
            build: "media-playback-test".into(),
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

    fn media(&mut self, operation: MediaOperation) -> MediaStatus {
        match self.command(&CommandPayload::Media {
            input: wire(2),
            operation,
        }) {
            EventPayload::MediaChanged {
                operation: changed,
                status,
                ..
            } => {
                assert_eq!(changed, operation);
                *status
            }
            other => panic!("expected media_changed, got {other:?}"),
        }
    }

    /// Collects `media` runtime events until `until` holds for one.
    #[cfg(unix)]
    fn media_until(
        &mut self,
        what: &str,
        mut until: impl FnMut(&[MediaEvent], &MediaStatus) -> bool,
    ) -> Vec<(u64, Vec<MediaEvent>, MediaStatus)> {
        let mut seen = Vec::new();
        self.wait_for(what, |message| {
            let WireMessage::RuntimeEvent(event) = message else {
                return None;
            };
            let RuntimeLifecycleEvent::Media {
                frame,
                events,
                status,
            } = event.event
            else {
                return None;
            };
            let done = until(&events, &status);
            seen.push((frame, events, *status));
            done.then_some(())
        });
        seen
    }
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn simulated_media_inputs_play_in_wall_time_report_state_and_persist_settings() {
    let directory = TestDirectory::new("transport");
    let project = directory.project();
    save_project(
        &project,
        "asset://clip.mkv",
        MediaPlaybackSettings::default(),
    );
    let daemon = Daemon::start(&project);
    let (mut client, snapshot, states) = Client::connect(&daemon);
    assert_eq!(snapshot.media_states, 1);
    let transport = &states[0].status.transport;
    assert_eq!(states[0].status.input, wire(2));
    assert!(transport.available);
    assert_eq!(transport.state, MediaTransportState::Stopped);
    assert_eq!((transport.position, transport.frames), (0, 250));
    assert_eq!(transport.audio_track, Some(1));
    assert_eq!(
        transport
            .tracks
            .iter()
            .map(|track| (track.stream_index, track.language.as_deref()))
            .collect::<Vec<_>>(),
        [(1, Some("eng")), (2, Some("spa"))]
    );

    // Settings are project edits; the transport cues to the new trim.
    let trimmed = client.media(MediaOperation::Trim {
        trim_in: 10,
        trim_out: Some(60),
    });
    assert_eq!(
        (trimmed.settings.trim_in, trimmed.settings.trim_out),
        (10, Some(60))
    );
    assert_eq!(trimmed.transport.playable_end, 60);
    client.media(MediaOperation::EndTriggerLead(Some(5)));
    client.media(MediaOperation::AudioTrack(Some(2)));

    // Playing runs in wall time to the trimmed end: the end trigger fires
    // five frames before it, then the clip ends and holds its last frame.
    let started = Instant::now();
    let playing = client.media(MediaOperation::Play);
    assert_eq!(playing.transport.state, MediaTransportState::Playing);
    let seen = client.media_until("the end of the clip", |events, _| {
        events.contains(&MediaEvent::Ended {
            action: MediaEndAction::Hold,
        })
    });
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1_500),
        "50 frames at 25 fps played in {elapsed:?}"
    );
    let positions = seen
        .iter()
        .map(|(_, _, status)| status.transport.position)
        .collect::<Vec<_>>();
    assert!(
        positions.windows(2).all(|pair| pair[0] <= pair[1]),
        "{positions:?}"
    );
    let all_events = seen
        .iter()
        .flat_map(|(_, events, _)| events.clone())
        .collect::<Vec<_>>();
    assert!(
        all_events.contains(&MediaEvent::EndTrigger {
            remaining_frames: 5
        }),
        "{all_events:?}"
    );
    let (ended_frame, _, ended) = seen.last().unwrap();
    assert_eq!(ended.transport.state, MediaTransportState::Ended);
    assert_eq!(ended.transport.audio_track, Some(2));
    // The trigger fired on the frame five before the end.
    let trigger_frame = seen
        .iter()
        .find(|(_, events, _)| {
            events
                .iter()
                .any(|event| matches!(event, MediaEvent::EndTrigger { .. }))
        })
        .unwrap()
        .0;
    assert_eq!(ended_frame - trigger_frame, 5);

    // Seek by rational time, rate, and loop.
    let seeked = client.media(MediaOperation::Seek(MediaSeekTarget::Time {
        numerator: 6,
        denominator: 5,
    }));
    assert_eq!(seeked.transport.position, 30);
    let fast = client.media(MediaOperation::Rate {
        speed_basis_points: 20_000,
    });
    assert_eq!(fast.settings.speed_basis_points, 20_000);
    client.media(MediaOperation::EndAction(MediaEndAction::Loop));
    client.media(MediaOperation::Play);
    client.media_until("a loop", |events, _| {
        events.contains(&MediaEvent::Looped { count: 1 })
    });
    let paused = client.media(MediaOperation::Pause);
    assert_eq!(paused.transport.state, MediaTransportState::Paused);
    drop(client);
    daemon.terminate();

    // Settings persist; the transport does not (it cues stopped at the trim).
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let settings = stored.project().media_playback(input(2)).copied().unwrap();
    assert_eq!(
        (
            settings.trim_in,
            settings.trim_out,
            settings.end_action,
            settings.end_trigger_lead,
            settings.speed_basis_points,
            settings.audio_stream
        ),
        (
            10,
            Some(60),
            fm_model::MediaEndAction::Loop,
            Some(5),
            20_000,
            Some(2)
        )
    );
    let daemon = Daemon::start(&project);
    let (_client, _, states) = Client::connect(&daemon);
    let restored = &states[0].status;
    assert_eq!(restored.settings.trim_in, 10);
    assert_eq!(restored.settings.speed_basis_points, 20_000);
    assert_eq!(restored.transport.state, MediaTransportState::Stopped);
    assert_eq!(restored.transport.position, 10);
    assert_eq!(restored.transport.audio_track, Some(2));
    daemon.stop();
}

#[test]
fn a_media_end_trigger_advances_the_playlist_it_airs_on_and_replays_after_restart() {
    let directory = TestDirectory::new("playlist");
    let project = directory.project();
    // The playlist airs an audio-only sting (IN-005): its media input plays
    // and triggers like any clip.
    save_project(
        &project,
        "asset://sting.wav",
        MediaPlaybackSettings {
            end_trigger_lead: Some(5),
            ..MediaPlaybackSettings::default()
        },
    );
    let daemon = Daemon::start(&project);
    let (mut client, _, _) = Client::connect(&daemon);
    let entries = vec![
        PlaylistEntrySpec {
            id: 1,
            input: wire(2),
            duration_frames: 10_000,
            follow_media: false,
            transition: PlaylistTransitionKind::Cut,
            transition_frames: 0,
            stinger_slot: None,
            enabled: true,
        },
        PlaylistEntrySpec {
            id: 2,
            input: wire(3),
            duration_frames: 10_000,
            follow_media: false,
            transition: PlaylistTransitionKind::Cut,
            transition_frames: 0,
            stinger_slot: None,
            enabled: true,
        },
    ];
    client.command(&CommandPayload::SetPlaylist {
        loop_mode: PlaylistLoopMode::Off,
        entries,
    });
    client.media(MediaOperation::Seek(MediaSeekTarget::Frame(230)));
    client.command(&CommandPayload::PlaylistControl {
        operation: PlaylistOperation::Start {
            at: PlaylistStart::Now,
            from: None,
        },
    });
    client.media(MediaOperation::Play);
    // Fifteen frames later the clip's end trigger fires and the daemon
    // takes the playlist's next entry long before its duration elapses.
    let (state, program) = client.wait_for("the playlist's next entry", |message| {
        let WireMessage::Event(event) = message else {
            return None;
        };
        let EventPayload::PlaylistChanged { state, program, .. } = event.payload else {
            return None;
        };
        matches!(state, PlaylistRunState::Playing { entry: 2, .. }).then_some((state, program))
    });
    assert!(matches!(state, PlaylistRunState::Playing { entry: 2, .. }));
    assert_eq!(program, wire(3));
    drop(client);
    daemon.stop();

    // The journaled advance replays: the restarted show airs entry 2.
    let daemon = Daemon::start(&project);
    let (_client, snapshot, _) = Client::connect(&daemon);
    assert_eq!(snapshot.realized_program, wire(3));
    daemon.stop();
}
