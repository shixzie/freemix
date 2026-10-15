use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fm_model::{
    AudioBus, BusSend, Input, InputAudioStripState, InputKind, Layer, LayerEffect, LayerGeometry,
    RectMask, Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
    SourceRef, StartupPolicy, StreamLadderRung, StreamProtocol,
};
use fm_persistence::{MutationBatch, ProjectStore, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CapabilityReportSummary, ClientType, CommandMessage, CommandPayload,
    CommandResult, EngineIdentity, EventCursor, EventMessage, EventPayload, HandshakeOutcome,
    HandshakeResponse, MAX_LINE_BYTES, OverlayStatus, ProtocolVersion, Role, RuntimeDomainBoundary,
    RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage, SnapshotReason,
    WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{BusId, InputId, OutputId, SceneId};

static TEST_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const TEST_PEER_TIMEOUT: Duration = Duration::from_secs(4);
const TEST_CLI_TIMEOUT: Duration = Duration::from_secs(4);

struct FakeRemoteServer {
    address: SocketAddr,
    worker: JoinHandle<()>,
}

impl FakeRemoteServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_remote_sessions(&listener));
        Self { address, worker }
    }

    fn start_peer_event_interleave() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_peer_event_interleave(&listener));
        Self { address, worker }
    }

    fn start_silent_response() -> Self {
        Self::start_test_peer(|stream| {
            stream.set_read_timeout(Some(TEST_PEER_TIMEOUT)).unwrap();
            let mut reader = BufReader::new(stream);
            assert_handshake_request_viewer(read_message(&mut reader));
            let mut byte = [0];
            assert_eq!(reader.get_mut().read(&mut byte).unwrap(), 0);
        })
    }

    fn start_diagnostics(response_log_id: &'static str) -> Self {
        Self::start_test_peer(move |stream| serve_diagnostics_peer(stream, response_log_id))
    }

    fn start_unterminated_response() -> Self {
        Self::start_test_peer(|stream| {
            stream.set_read_timeout(Some(TEST_PEER_TIMEOUT)).unwrap();
            stream.set_write_timeout(Some(TEST_PEER_TIMEOUT)).unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            assert_handshake_request_viewer(read_message(&mut reader));
            if let Err(error) = writer.write_all(&vec![b'x'; MAX_LINE_BYTES + 1]) {
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ));
            }
            let mut byte = [0];
            let _ = reader.get_mut().read(&mut byte);
        })
    }

    fn start_test_peer(handler: impl FnOnce(TcpStream) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || handler(accept_test_peer(&listener)));
        Self { address, worker }
    }

    fn start_alpha_fade() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_alpha_fade(&listener));
        Self { address, worker }
    }

    fn start_slide() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_slide(&listener));
        Self { address, worker }
    }

    fn start_zoom() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_zoom(&listener));
        Self { address, worker }
    }

    fn start_cube() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            serve_automatic_transition(
                &listener,
                fm_protocol::CURRENT_PROTOCOL_VERSION,
                &CommandPayload::Transition {
                    kind: fm_protocol::AutoTransitionKind::Cube,
                    duration_frames: 3,
                },
                "remote-cube",
            );
        });
        Self { address, worker }
    }

    fn start_audio_strip() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_audio_strip(&listener));
        Self { address, worker }
    }

    fn start_stinger() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_stinger(&listener));
        Self { address, worker }
    }

    fn start_stinger_configuration() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_stinger_configuration(&listener));
        Self { address, worker }
    }

    fn start_fade_to_black() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_fade_to_black(&listener));
        Self { address, worker }
    }

    fn start_stream_start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_stream_start(&listener));
        Self { address, worker }
    }

    fn start_record_start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_record_start(&listener));
        Self { address, worker }
    }

    fn start_manual_position() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_manual_position(&listener));
        Self { address, worker }
    }

    fn start_manual_alpha_fade() -> Self {
        Self::start_manual(
            fm_protocol::ManualTransitionKind::AlphaFade,
            "remote-manual-alpha",
        )
    }

    fn start_manual_slide() -> Self {
        Self::start_manual(
            fm_protocol::ManualTransitionKind::Slide,
            "remote-manual-slide",
        )
    }

    fn start_manual(kind: fm_protocol::ManualTransitionKind, key: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_manual_start(&listener, kind, key));
        Self { address, worker }
    }

    fn start_media_seek() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || serve_media_seek(&listener));
        Self { address, worker }
    }

    fn address(&self) -> String {
        self.address.to_string()
    }

    fn finish(self) {
        self.worker.join().unwrap();
    }
}

fn accept_test_peer(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + TEST_PEER_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::yield_now();
            }
            Err(error) => panic!("test peer did not connect: {error}"),
        }
    }
}

fn serve_remote_sessions(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let mut revision = 0;

    for session in 0..5 {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let role = if matches!(session, 0 | 4) {
            Role::Viewer
        } else {
            Role::Operator
        };
        assert_handshake_request_role(read_message(&mut reader), role);
        write_handshake_role(&mut writer, &engine, revision, role);

        if matches!(session, 0 | 4) {
            continue;
        }

        let WireMessage::Command(command) = read_message(&mut reader) else {
            panic!("expected remote command");
        };
        match session {
            1 => assert_command(
                &command,
                &CommandPayload::SelectPreview { input: input(1) },
                "remote-preview",
                0,
            ),
            2 => assert_command(&command, &CommandPayload::Cut, "remote-cut", 1),
            3 => assert_command(
                &command,
                &CommandPayload::Fade { duration_frames: 4 },
                "remote-fade",
                2,
            ),
            _ => unreachable!(),
        }

        revision += 1;
        let result = CommandResult::Accepted {
            id: command.id.clone(),
            revision,
            scheduled_frame: Some(revision),
        };
        write_message(&mut writer, &WireMessage::CommandResult(result));
        write_command_events(
            &mut writer,
            &engine,
            revision,
            &command.payload,
            input(1),
            input(1),
        );
    }
}

fn serve_diagnostics_peer(stream: TcpStream, response_log_id: &str) {
    let engine = EngineIdentity {
        engine_id: "diag-engine".into(),
        state_epoch: 7,
        log_id: "secret-log".into(),
    };
    stream.set_read_timeout(Some(TEST_PEER_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TEST_PEER_TIMEOUT)).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let hello = read_message(&mut reader);
    assert_handshake_request_viewer(hello);
    write_handshake_role(&mut writer, &engine, 0, Role::Viewer);
    let WireMessage::DiagnosticsRequest(request) = read_message(&mut reader) else {
        panic!("expected diagnostics request");
    };
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 0,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
    let mut response_engine = engine.clone();
    response_engine.log_id = response_log_id.into();
    write_message(
        &mut writer,
        &WireMessage::DiagnosticsResponse(fm_protocol::DiagnosticsResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: request.request_id,
            engine: response_engine,
            current_revision: 12,
            oldest_retained_revision: Some(3),
            newest_retained_revision: Some(12),
            subscriber_count: 2,
            retained_events_limit: 64,
            subscriber_limit: 8,
            subscriber_queue_limit: 16,
        }),
    );
}

fn assert_handshake_request_viewer(message: WireMessage) {
    assert_handshake_request_role(message, Role::Viewer);
}

fn assert_handshake_request_role(message: WireMessage, expected_role: Role) {
    let WireMessage::HandshakeRequest(hello) = message else {
        panic!("expected handshake request");
    };
    assert_eq!(hello.desired_role, expected_role);
    assert_eq!(hello.client_type, ClientType::Cli);
    assert_eq!(hello.protocol, CURRENT_PROTOCOL_VERSION);
    assert_eq!(hello.resume_cursor, None);
}

fn serve_peer_event_interleave(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake(&mut writer, &engine, 0);
    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote command");
    };
    assert_eq!(
        command.payload,
        CommandPayload::SelectPreview { input: input(2) }
    );
    assert_eq!(command.idempotency_key, "peer-event-interleave");
    assert_eq!(command.expected_revision, None);
    write_command_events(
        &mut writer,
        &engine,
        1,
        &CommandPayload::Cut,
        input(2),
        input(1),
    );
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 2,
            scheduled_frame: Some(2),
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 2,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(2),
                preview: input(2),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }),
    );
    for (sequence, domain) in [(1, "audio"), (2, "switcher")] {
        write_message(
            &mut writer,
            &WireMessage::RuntimeEvent(RuntimeEventMessage {
                server: server_identity(&engine),
                revision: 2,
                generation: 2,
                sequence,
                event: RuntimeLifecycleEvent::Realized {
                    domain: domain.into(),
                    manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                    fade_to_black: live_fade_to_black(),
                },
            }),
        );
    }
}

fn serve_fade_to_black(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake_version_with_fade_to_black(
        &mut writer,
        &engine,
        0,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        live_fade_to_black(),
    );

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote FTB command");
    };
    assert_eq!(command.protocol, fm_protocol::CURRENT_PROTOCOL_VERSION);
    assert_eq!(
        command.payload,
        CommandPayload::FadeToBlack {
            active: true,
            duration_frames: 2,
        }
    );
    assert_eq!(command.idempotency_key, "remote-blackout");
    assert_eq!(command.expected_revision, Some(0));
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: Some(1),
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: fm_protocol::FadeToBlackState {
                    target_active: true,
                    position: fm_protocol::FadeToBlackPosition::LIVE,
                },
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: fm_protocol::FadeToBlackState {
                    target_active: true,
                    position: fm_protocol::FadeToBlackPosition::BLACK,
                },
            },
        }),
    );
}

fn serve_alpha_fade(listener: &TcpListener) {
    serve_automatic_transition(
        listener,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        &CommandPayload::AlphaFade { duration_frames: 3 },
        "remote-alpha-fade",
    );
}

/// Serves one Operator `StreamStart` command session followed by one Viewer
/// session whose snapshot carries the durable streams roster.
fn serve_stream_start(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let target = fm_protocol::WireStreamTargetId::new(NonZeroU128::new(40).unwrap());
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake_role(&mut writer, &engine, 0, Role::Operator);

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote stream command");
    };
    assert_command(
        &command,
        &CommandPayload::StreamStart { target },
        "remote-stream-start",
        0,
    );
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: None,
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::StreamStatus(fm_protocol::StreamStatusMessage {
            server: server_identity(&engine),
            sequence: 1,
            samples: vec![fm_protocol::StreamStatusSample {
                target,
                realized: fm_protocol::StreamRealizedState::Starting,
                connected: false,
                muxed_bytes: 2048,
                enqueued_pairs: 12,
                dropped_pairs: 3,
                failure: None,
                local: None,
            }],
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::StreamsChanged {
                streams: vec![fm_protocol::StreamStatus {
                    target,
                    name: "Main ingest".into(),
                    desired_running: true,
                    realized: fm_protocol::StreamRealizedState::Starting,
                    detail: Some("connecting to ingest".into()),
                }],
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
    serve_streams_snapshot_peer(
        listener,
        &engine,
        vec![fm_protocol::StreamStatus {
            target,
            name: "Main ingest".into(),
            desired_running: true,
            realized: fm_protocol::StreamRealizedState::Live,
            detail: None,
        }],
        recording_roster([false, false]),
    );
}

/// The fixed two-slot recorder roster carried by test snapshots, in slot
/// order: slot 1 first, then slot 2.
fn recording_roster(desired: [bool; 2]) -> Vec<fm_protocol::RecordingStatus> {
    desired
        .into_iter()
        .enumerate()
        .map(|(index, desired_active)| fm_protocol::RecordingStatus {
            recorder: wire_recorder(u8::try_from(index + 1).expect("two recorder slots fit in u8")),
            desired_active,
        })
        .collect()
}

/// The wire recorder slot for `number`, which tests only call with 1 or 2.
fn wire_recorder(number: u8) -> fm_protocol::WireRecorderSlot {
    fm_protocol::WireRecorderSlot::new(number).expect("test recorder slots are 1 or 2")
}

/// Serves one Operator `RecordStart` command session followed by one Viewer
/// session whose snapshot carries the durable per-slot recording roster.
fn serve_record_start(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake_role(&mut writer, &engine, 0, Role::Operator);

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote record command");
    };
    assert_command(
        &command,
        &CommandPayload::RecordStart {
            recorder: wire_recorder(1),
        },
        "remote-record-start",
        0,
    );
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: None,
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::RecordingChanged {
                recorder: wire_recorder(1),
                active: true,
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
    serve_streams_snapshot_peer(
        listener,
        &engine,
        Vec::new(),
        recording_roster([true, false]),
    );
}

/// Serves a Viewer session whose initial snapshot reports the durable streams
/// roster and per-slot recording roster.
fn serve_streams_snapshot_peer(
    listener: &TcpListener,
    engine: &EngineIdentity,
    streams: Vec<fm_protocol::StreamStatus>,
    recordings: Vec<fm_protocol::RecordingStatus>,
) {
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request_viewer(read_message(&mut reader));
    write_message(
        &mut writer,
        &WireMessage::HandshakeResponse(HandshakeResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            granted_role: Role::Viewer,
            permissions: handshake_permissions(Role::Viewer),
            capabilities: CapabilityReportSummary {
                digest: "fake-capabilities".into(),
                total: 0,
                available: 0,
                degraded: 0,
                unavailable: 0,
            },
            server: server_identity(engine),
            current_revision: 1,
            outcome: HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Snapshot(SnapshotMessage {
            engine: engine.clone(),
            revision: 1,
            show_name: "Remote Contract".into(),
            inputs: input_statuses(),
            outputs: output_statuses(),
            input_audio_strips: input_audio_strips(),
            desired_program: input(1),
            desired_preview: input(2),
            realized_program: input(1),
            realized_preview: input(2),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: live_fade_to_black(),
            realized_fade_to_black: live_fade_to_black(),
            stingers: Vec::new(),
            streams,
            recordings,
            desired_overlays: OverlayStatus::empty_channels(),
            realized_overlays: OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: Vec::new(),
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        }),
    );
}

fn serve_slide(listener: &TcpListener) {
    serve_automatic_transition(
        listener,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        &CommandPayload::Slide { duration_frames: 3 },
        "remote-slide",
    );
}

fn serve_zoom(listener: &TcpListener) {
    serve_automatic_transition(
        listener,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        &CommandPayload::Zoom { duration_frames: 3 },
        "remote-zoom",
    );
}

fn serve_audio_strip(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request_role(read_message(&mut reader), Role::Audio);
    write_handshake_role(&mut writer, &engine, 0, Role::Audio);

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote input-audio-strip command");
    };
    assert_command(
        &command,
        &CommandPayload::SetInputAudioStrip {
            input: input(2),
            gain_millidb: -6_000,
            balance_basis_points: 2_500,
            muted: true,
            soloed: true,
            follow_video: false,
            delay_samples: 2_400,
        },
        "remote-audio-strip",
        0,
    );
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: Some(1),
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: vec![
                    fm_protocol::InputAudioStripStatus {
                        input: input(1),
                        gain_millidb: 0,
                        balance_basis_points: 0,
                        muted: false,
                        soloed: false,
                        follow_video: true,
                        delay_samples: 0,
                    },
                    fm_protocol::InputAudioStripStatus {
                        input: input(2),
                        gain_millidb: -6_000,
                        balance_basis_points: 2_500,
                        muted: true,
                        soloed: true,
                        follow_video: false,
                        delay_samples: 2_400,
                    },
                ],
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "audio".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
}

fn serve_stinger(listener: &TcpListener) {
    serve_automatic_transition(
        listener,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        &CommandPayload::Stinger {
            slot: fm_protocol::WireStingerSlotId::new(8).unwrap(),
            duration_frames: 3,
        },
        "remote-stinger",
    );
}

fn serve_stinger_configuration(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let expected = [
        CommandPayload::ConfigureStinger {
            slot: fm_protocol::WireStingerSlotId::new(3).unwrap(),
            media_input: input(2),
            preload: true,
            cut_point_frames: 45,
            audio_policy: fm_protocol::StingerAudioPolicy::MixWithProgram,
            missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Fade,
        },
        CommandPayload::RemoveStinger {
            slot: fm_protocol::WireStingerSlotId::new(3).unwrap(),
        },
    ];
    for (index, expected_payload) in expected.into_iter().enumerate() {
        let revision = index as u64;
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        assert_handshake_request(read_message(&mut reader));
        write_handshake(&mut writer, &engine, revision);
        let WireMessage::Command(command) = read_message(&mut reader) else {
            panic!("expected stinger command");
        };
        assert_command(
            &command,
            &expected_payload,
            if index == 0 {
                "configure-stinger"
            } else {
                "remove-stinger"
            },
            revision,
        );
        let new_revision = revision + 1;
        write_message(
            &mut writer,
            &WireMessage::CommandResult(CommandResult::Accepted {
                id: command.id,
                revision: new_revision,
                scheduled_frame: None,
            }),
        );
        let cursor = EventCursor {
            engine: engine.clone(),
            revision: new_revision,
        };
        write_message(
            &mut writer,
            &WireMessage::Event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor,
                payload: EventPayload::StingerSlotsChanged {
                    program: input(1),
                    preview: input(2),
                    manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                    fade_to_black: live_fade_to_black(),
                    stingers: Vec::new(),
                    overlays: OverlayStatus::empty_channels(),
                    input_audio_strips: input_audio_strips(),
                },
            }),
        );
        write_message(
            &mut writer,
            &WireMessage::RuntimeEvent(RuntimeEventMessage {
                server: server_identity(&engine),
                revision: new_revision,
                generation: new_revision,
                sequence: 1,
                event: RuntimeLifecycleEvent::Realized {
                    domain: "switcher".into(),
                    manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                    fade_to_black: live_fade_to_black(),
                },
            }),
        );
    }
}

fn serve_automatic_transition(
    listener: &TcpListener,
    protocol: ProtocolVersion,
    expected_payload: &CommandPayload,
    expected_key: &str,
) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake_version_with_fade_to_black(
        &mut writer,
        &engine,
        0,
        protocol,
        live_fade_to_black(),
    );

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote automatic transition command");
    };
    assert_eq!(command.protocol, protocol);
    assert_eq!(&command.payload, expected_payload);
    assert_eq!(command.idempotency_key, expected_key);
    assert_eq!(command.expected_revision, Some(0));
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: Some(1),
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(2),
                preview: input(1),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Scheduled {
                domains: vec![RuntimeDomainBoundary {
                    domain: "switcher".into(),
                    boundary: 1,
                }],
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 2,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
}

/// A media input (2) cued at frame 0 of a 250-frame file with two tracks.
fn media_status(position: u64, time: (u128, u128)) -> fm_protocol::MediaStatus {
    let track = |stream_index: u32, language: &str, default: bool| fm_protocol::MediaAudioTrack {
        stream_index,
        sample_rate: 48_000,
        channels: 2,
        language: Some(language.into()),
        title: None,
        default,
    };
    fm_protocol::MediaStatus {
        input: input(2),
        settings: fm_protocol::MediaSettingsStatus::default(),
        transport: fm_protocol::MediaTransportStatus {
            available: true,
            state: fm_protocol::MediaTransportState::Stopped,
            position,
            position_time: fm_protocol::MediaTime {
                numerator: time.0,
                denominator: time.1,
            },
            playable_end: 250,
            frames: 250,
            complete: true,
            loops: 0,
            audio_track: Some(1),
            tracks: vec![track(1, "eng", true), track(2, "spa", false)],
            source_rate: Some(fm_types::FrameRate::new(25, 1).unwrap()),
            fault: None,
        },
    }
}

/// IN-003: a remote seek is an operator command that completes on
/// `media_changed` and media realization; the snapshot's `media_state`
/// completes the initial sync.
fn serve_media_seek(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake_snapshot(
        &mut writer,
        &engine,
        (0, CURRENT_PROTOCOL_VERSION),
        fm_protocol::ManualTransitionStatus::Inactive,
        Role::Operator,
        1,
    );
    write_message(
        &mut writer,
        &WireMessage::MediaState(fm_protocol::MediaStateMessage {
            revision: 0,
            frame: 0,
            status: media_status(0, (0, 1)),
        }),
    );
    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected the remote media seek");
    };
    let payload = CommandPayload::Media {
        input: input(2),
        operation: fm_protocol::MediaOperation::Seek(fm_protocol::MediaSeekTarget::Time {
            numerator: 6,
            denominator: 5,
        }),
    };
    assert_command(&command, &payload, "remote-media-seek", 0);
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: Some(1),
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::MediaChanged {
                frame: 1,
                operation: fm_protocol::MediaOperation::Seek(fm_protocol::MediaSeekTarget::Time {
                    numerator: 6,
                    denominator: 5,
                }),
                status: Box::new(media_status(30, (6, 5))),
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "media".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
}

fn serve_manual_start(
    listener: &TcpListener,
    kind: fm_protocol::ManualTransitionKind,
    expected_key: &str,
) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    write_handshake_version_with_fade_to_black(
        &mut writer,
        &engine,
        0,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        live_fade_to_black(),
    );

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote manual transition command");
    };
    assert_eq!(command.protocol, fm_protocol::CURRENT_PROTOCOL_VERSION);
    assert_eq!(
        command.payload,
        CommandPayload::StartManualTransition { kind }
    );
    assert_eq!(command.idempotency_key, expected_key);
    assert_eq!(command.expected_revision, Some(0));
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: Some(1),
        }),
    );
    let manual_transition =
        fm_protocol::ManualTransitionStatus::Active(fm_protocol::ManualTransitionState {
            kind,
            from: input(1),
            to: input(2),
            interval_start: fm_protocol::ManualTransitionPosition::START,
            position: fm_protocol::ManualTransitionPosition::START,
        });
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition,
                fade_to_black: live_fade_to_black(),
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }),
    );
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
}

fn serve_manual_position(listener: &TcpListener) {
    let engine = EngineIdentity {
        engine_id: "project-42".into(),
        state_epoch: 1,
        log_id: "fake-remote-log".into(),
    };
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    assert_handshake_request(read_message(&mut reader));
    let initial_manual =
        fm_protocol::ManualTransitionStatus::Active(fm_protocol::ManualTransitionState {
            kind: fm_protocol::ManualTransitionKind::Fade,
            from: input(1),
            to: input(2),
            interval_start: fm_protocol::ManualTransitionPosition::START,
            position: fm_protocol::ManualTransitionPosition::START,
        });
    write_handshake_version_with_manual(
        &mut writer,
        &engine,
        0,
        fm_protocol::CURRENT_PROTOCOL_VERSION,
        initial_manual,
        Role::Operator,
    );

    let WireMessage::Command(command) = read_message(&mut reader) else {
        panic!("expected remote manual-position command");
    };
    assert_eq!(command.protocol, fm_protocol::CURRENT_PROTOCOL_VERSION);
    assert_eq!(
        command.payload,
        CommandPayload::SetManualTransitionPosition {
            position: fm_protocol::ManualTransitionPosition::END,
        }
    );
    assert_eq!(command.idempotency_key, "manual-endpoint");
    assert_eq!(command.expected_revision, Some(0));
    write_message(
        &mut writer,
        &WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 1,
            scheduled_frame: Some(1),
        }),
    );
    let desired_state = fm_protocol::ManualTransitionState {
        kind: fm_protocol::ManualTransitionKind::Fade,
        from: input(1),
        to: input(2),
        interval_start: fm_protocol::ManualTransitionPosition::START,
        position: fm_protocol::ManualTransitionPosition::END,
    };
    write_message(
        &mut writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 1,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: fm_protocol::ManualTransitionStatus::Active(desired_state),
                fade_to_black: live_fade_to_black(),
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }),
    );
    let realized =
        fm_protocol::ManualTransitionStatus::Active(fm_protocol::ManualTransitionState {
            interval_start: fm_protocol::ManualTransitionPosition::END,
            ..desired_state
        });
    write_message(
        &mut writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(&engine),
            revision: 1,
            generation: 1,
            sequence: 1,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: realized,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
}

fn assert_handshake_request(message: WireMessage) {
    assert_handshake_request_role(message, Role::Operator);
}

fn write_handshake(writer: &mut TcpStream, engine: &EngineIdentity, revision: u64) {
    write_handshake_role(writer, engine, revision, Role::Operator);
}

fn write_handshake_role(
    writer: &mut TcpStream,
    engine: &EngineIdentity,
    revision: u64,
    role: Role,
) {
    write_handshake_version_with_manual(
        writer,
        engine,
        revision,
        CURRENT_PROTOCOL_VERSION,
        fm_protocol::ManualTransitionStatus::Inactive,
        role,
    );
}

fn write_handshake_version_with_manual(
    writer: &mut TcpStream,
    engine: &EngineIdentity,
    revision: u64,
    protocol: ProtocolVersion,
    manual_transition: fm_protocol::ManualTransitionStatus,
    role: Role,
) {
    write_handshake_snapshot(
        writer,
        engine,
        (revision, protocol),
        manual_transition,
        role,
        0,
    );
}

fn write_handshake_snapshot(
    writer: &mut TcpStream,
    engine: &EngineIdentity,
    (revision, protocol): (u64, ProtocolVersion),
    manual_transition: fm_protocol::ManualTransitionStatus,
    role: Role,
    media_states: u16,
) {
    let permissions = handshake_permissions(role);
    write_message(
        writer,
        &WireMessage::HandshakeResponse(HandshakeResponse {
            protocol,
            granted_role: role,
            permissions,
            capabilities: CapabilityReportSummary {
                digest: "fake-capabilities".into(),
                total: 0,
                available: 0,
                degraded: 0,
                unavailable: 0,
            },
            server: server_identity(engine),
            current_revision: revision,
            outcome: HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        }),
    );
    let preview = if revision == 0 { input(2) } else { input(1) };
    write_message(
        writer,
        &WireMessage::Snapshot(SnapshotMessage {
            engine: engine.clone(),
            revision,
            show_name: "Remote Contract".into(),
            inputs: input_statuses(),
            outputs: output_statuses(),
            input_audio_strips: input_audio_strips(),
            desired_program: input(1),
            desired_preview: preview,
            realized_program: input(1),
            realized_preview: preview,
            desired_manual_transition: manual_transition,
            realized_manual_transition: manual_transition,
            desired_fade_to_black: live_fade_to_black(),
            realized_fade_to_black: live_fade_to_black(),
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: recording_roster([false, false]),
            desired_overlays: OverlayStatus::empty_channels(),
            realized_overlays: OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: Vec::new(),
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states,
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        }),
    );
}

fn handshake_permissions(role: Role) -> Vec<String> {
    match role {
        Role::Viewer => vec!["view_status".into()],
        Role::Audio => ["view_status", "control_audio"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        Role::Operator => [
            "view_status",
            "select_preview",
            "transition",
            "control_audio",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        _ => unreachable!("test handshake role must be Viewer, Audio, or Operator"),
    }
}

fn write_handshake_version_with_fade_to_black(
    writer: &mut TcpStream,
    engine: &EngineIdentity,
    revision: u64,
    protocol: ProtocolVersion,
    fade_to_black: fm_protocol::FadeToBlackState,
) {
    write_message(
        writer,
        &WireMessage::HandshakeResponse(HandshakeResponse {
            protocol,
            granted_role: Role::Operator,
            permissions: handshake_permissions(Role::Operator),
            capabilities: CapabilityReportSummary {
                digest: "fake-capabilities".into(),
                total: 0,
                available: 0,
                degraded: 0,
                unavailable: 0,
            },
            server: server_identity(engine),
            current_revision: revision,
            outcome: HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        }),
    );
    write_message(
        writer,
        &WireMessage::Snapshot(SnapshotMessage {
            engine: engine.clone(),
            revision,
            show_name: "Remote Contract".into(),
            inputs: input_statuses(),
            outputs: Vec::new(),
            input_audio_strips: input_audio_strips(),
            desired_program: input(1),
            desired_preview: input(2),
            realized_program: input(1),
            realized_preview: input(2),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fade_to_black,
            realized_fade_to_black: fade_to_black,
            desired_overlays: OverlayStatus::empty_channels(),
            realized_overlays: OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: recording_roster([false, false]),
            titles: Vec::new(),
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        }),
    );
}

fn live_fade_to_black() -> fm_protocol::FadeToBlackState {
    fm_protocol::FadeToBlackState {
        target_active: false,
        position: fm_protocol::FadeToBlackPosition::LIVE,
    }
}

fn assert_command(
    command: &CommandMessage,
    payload: &CommandPayload,
    key: &str,
    expected_revision: u64,
) {
    assert_eq!(command.protocol, CURRENT_PROTOCOL_VERSION);
    assert_eq!(command.idempotency_key, key);
    assert_eq!(command.expected_revision, Some(expected_revision));
    assert_eq!(command.deadline_ms, None);
    assert_eq!(&command.payload, payload);
}

fn write_command_events(
    writer: &mut TcpStream,
    engine: &EngineIdentity,
    revision: u64,
    payload: &CommandPayload,
    program: WireInputId,
    preview: WireInputId,
) {
    let cursor = EventCursor {
        engine: engine.clone(),
        revision,
    };
    write_message(
        writer,
        &WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor,
            payload: EventPayload::DesiredSwitcher {
                program,
                preview,
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }),
    );
    let generation = revision;
    let mut sequence = 1;
    if matches!(
        payload,
        CommandPayload::Fade { duration_frames } if *duration_frames > 1
    ) {
        write_message(
            writer,
            &WireMessage::RuntimeEvent(RuntimeEventMessage {
                server: server_identity(engine),
                revision,
                generation,
                sequence,
                event: RuntimeLifecycleEvent::Scheduled {
                    domains: vec![RuntimeDomainBoundary {
                        domain: "switcher".into(),
                        boundary: revision,
                    }],
                },
            }),
        );
        sequence += 1;
    }
    write_message(
        writer,
        &WireMessage::RuntimeEvent(RuntimeEventMessage {
            server: server_identity(engine),
            revision,
            generation,
            sequence,
            event: RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        }),
    );
}

fn server_identity(engine: &EngineIdentity) -> ServerIdentity {
    ServerIdentity {
        engine_id: engine.engine_id.clone(),
        project_id: "42".into(),
        state_epoch: engine.state_epoch,
        log_id: engine.log_id.clone(),
    }
}

fn read_message(reader: &mut impl BufRead) -> WireMessage {
    let mut line = String::new();
    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
    decode_line(&line).unwrap()
}

fn write_message(writer: &mut TcpStream, message: &WireMessage) {
    writer
        .write_all(encode_line(message).unwrap().as_bytes())
        .unwrap();
    writer.flush().unwrap();
}

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn input_statuses() -> Vec<fm_protocol::InputStatus> {
    [(1, "Camera"), (2, "Slides")]
        .into_iter()
        .map(|(value, name)| fm_protocol::InputStatus {
            input: input(value),
            name: name.into(),
        })
        .collect()
}

fn output_statuses() -> Vec<fm_protocol::OutputStatus> {
    [(30, "Program \"Main\""), (31, "Auxiliary")]
        .into_iter()
        .map(|(value, name)| fm_protocol::OutputStatus {
            output: fm_protocol::WireOutputId::new(NonZeroU128::new(value).unwrap()),
            name: name.into(),
        })
        .collect()
}

fn input_audio_strips() -> Vec<fm_protocol::InputAudioStripStatus> {
    [input(1), input(2)]
        .into_iter()
        .map(|input| fm_protocol::InputAudioStripStatus {
            input,
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        })
        .collect()
}

#[test]
fn remote_commands_use_protocol_server() {
    let server = FakeRemoteServer::start();
    let address = server.address();
    let initial = invoke(&["remote-status", &address]);
    assert_success(&initial);
    let initial_status = stdout(&initial);
    assert_remote_status(&initial_status, 0, 1, 1, 2, 2);
    assert!(initial_status.contains(
        r#"Inputs=[1:"Camera",2:"Slides"] Outputs=[30:"Program \"Main\"",31:"Auxiliary"]"#
    ));

    let preview = invoke(&[
        "remote-preview",
        &address,
        "1",
        "--key",
        "remote-preview",
        "--expect",
        "0",
    ]);
    assert_success(&preview);
    assert_remote_status(&stdout(&preview), 1, 1, 1, 1, 1);

    let cut = invoke(&[
        "remote-cut",
        &address,
        "--key",
        "remote-cut",
        "--expect",
        "1",
    ]);
    assert_success(&cut);
    assert_remote_status(&stdout(&cut), 2, 1, 1, 1, 1);

    let fade = invoke(&[
        "remote-fade",
        &address,
        "4",
        "--key",
        "remote-fade",
        "--expect",
        "2",
    ]);
    assert_success(&fade);
    let final_status = stdout(&fade);
    assert_remote_status(&final_status, 3, 1, 1, 1, 1);

    let current = invoke(&["remote-status", &address]);
    assert_success(&current);
    assert_eq!(stdout(&current), final_status);

    server.finish();
}

#[test]
fn remote_commands_reject_non_loopback_addresses_before_connecting() {
    let output = invoke(&["remote-status", "192.0.2.1:9123"]);
    assert_failure_contains(&output, "requires a loopback address");
}

#[test]
fn remote_status_times_out_when_peer_sends_no_response() {
    let server = FakeRemoteServer::start_silent_response();
    let output = invoke_bounded(&["remote-status", &server.address()]);
    assert_failure_contains(&output, "daemon response timed out");
    server.finish();
}

#[test]
fn remote_diagnostics_reports_exact_identity_and_rejects_mismatched_log_id() {
    let server = FakeRemoteServer::start_diagnostics("secret-log");
    let address = server.address();
    let output = invoke_bounded(&["remote-diagnostics", &address]);
    server.finish();
    assert_success(&output);
    let rendered = stdout(&output);
    assert_eq!(
        rendered,
        "diagnostics=v1 engine_id=diag-engine state_epoch=7 revision=12 retained_oldest=3 retained_newest=12 subscribers=2/8 retained_limit=64 subscriber_queue=16"
    );
    assert!(!rendered.contains("log_id"));

    let server = FakeRemoteServer::start_diagnostics("mismatched-log");
    let output = invoke_bounded(&["remote-diagnostics", &server.address()]);
    server.finish();
    assert_failure_contains(&output, "diagnostics response does not match the request");
}

#[test]
fn remote_status_rejects_an_unterminated_oversized_record() {
    let server = FakeRemoteServer::start_unterminated_response();
    let output = invoke_bounded(&["remote-status", &server.address()]);
    assert_failure_contains(&output, "protocol line exceeds maximum length");
    server.finish();
}

#[test]
fn remote_manual_alpha_fade_preserves_kind_protocol_and_replicated_state() {
    let server = FakeRemoteServer::start_manual_alpha_fade();
    let output = invoke(&[
        "remote-tbar-start",
        &server.address(),
        "alpha-fade",
        "--key",
        "remote-manual-alpha",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    assert!(
        stdout(&output).contains("TBar(desired=alpha_fade:1->2@0, realized=alpha_fade:1->2@0)")
    );
    server.finish();
}

#[test]
fn remote_manual_slide_preserves_kind_protocol_and_replicated_state() {
    let server = FakeRemoteServer::start_manual_slide();
    let output = invoke(&[
        "remote-tbar-start",
        &server.address(),
        "slide",
        "--key",
        "remote-manual-slide",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    assert!(stdout(&output).contains("TBar(desired=slide:1->2@0, realized=slide:1->2@0)"));
    server.finish();
}

#[test]
fn remote_alpha_fade_preserves_duration_and_protocol() {
    let server = FakeRemoteServer::start_alpha_fade();
    let output = invoke(&[
        "remote-alpha-fade",
        &server.address(),
        "3",
        "--key",
        "remote-alpha-fade",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    server.finish();
}

#[test]
fn remote_slide_preserves_duration_and_protocol() {
    let server = FakeRemoteServer::start_slide();
    let output = invoke(&[
        "remote-slide",
        &server.address(),
        "3",
        "--key",
        "remote-slide",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    server.finish();
}

#[test]
fn remote_zoom_preserves_duration_and_protocol() {
    let server = FakeRemoteServer::start_zoom();
    let output = invoke(&[
        "remote-zoom",
        &server.address(),
        "3",
        "--key",
        "remote-zoom",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    server.finish();
}

#[test]
fn remote_transition_sends_its_kind_and_duration() {
    let server = FakeRemoteServer::start_cube();
    let output = invoke(&[
        "remote-transition",
        &server.address(),
        "cube",
        "3",
        "--key",
        "remote-cube",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    server.finish();
}

#[test]
fn remote_input_media_seek_sends_a_rational_time_and_prints_the_media_status() {
    let server = FakeRemoteServer::start_media_seek();
    let output = invoke(&[
        "remote-input-media-seek",
        &server.address(),
        "2",
        "6/5s",
        "--key",
        "remote-media-seek",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    let printed = stdout(&output);
    assert!(
        printed.contains(
            "Media(input=2, available=true, state=stopped, position=30, time=6/5, end=250, frames=250, complete=true, loops=0, track=1, tracks=[1:eng*,2:spa], trim=0..end, speed_bp=10000, end_action=hold, end_trigger=off, audio_policy=mute, audio_stream=none, growing=none, start=0, fault=none)"
        ),
        "{printed}"
    );
    server.finish();
}

/// IN-003: the CLI edits a media input's persisted playback settings
/// offline and refuses transport, which only a running server has.
#[test]
fn local_input_media_settings_persist_and_transport_needs_a_server() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "media-input-add",
        context.project_path(),
        "9",
        "Clip",
        "asset://clip.mkv",
    ]));
    for arguments in [
        &["input-media-trim", "9", "10", "200"][..],
        &["input-media-rate", "9", "20000"],
        &["input-media-end-action", "9", "loop"],
        &["input-media-end-trigger", "9", "5"],
        &["input-media-audio-track", "9", "2"],
        &["input-media-audio-policy", "9", "resample"],
    ] {
        let mut command = vec![arguments[0], context.project_path()];
        command.extend_from_slice(&arguments[1..]);
        assert_success(&invoke(&command));
    }
    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let settings = stored
        .project()
        .media_playback(InputId::new(NonZeroU128::new(9).unwrap()))
        .copied()
        .unwrap();
    assert_eq!(
        settings,
        fm_model::MediaPlaybackSettings {
            trim_in: 10,
            trim_out: Some(200),
            end_action: fm_model::MediaEndAction::Loop,
            end_trigger_lead: Some(5),
            speed_basis_points: 20_000,
            audio_policy: fm_model::MediaAudioSpeedPolicy::Resample,
            audio_stream: Some(2),
            start_frame: 10,
            ..fm_model::MediaPlaybackSettings::default()
        }
    );
    let listed = invoke(&["input-media-list", context.project_path()]);
    assert_success(&listed);
    assert!(
        stdout(&listed).contains(
            "trim=10..200, speed_bp=20000, end_action=loop, end_trigger=5, audio_policy=resample, audio_stream=2"
        ),
        "{}",
        stdout(&listed)
    );

    assert_failure_contains(
        &invoke(&["input-media-play", context.project_path(), "9"]),
        "use remote-input-media-play",
    );
    assert!(
        !invoke(&["input-media-rate", context.project_path(), "9", "1000"])
            .status
            .success()
    );
    assert!(
        !invoke(&["input-media-trim", context.project_path(), "1", "0", "end"])
            .status
            .success()
    );
    assert_failure_contains(
        &invoke(&[
            "input-media-end-action",
            context.project_path(),
            "9",
            "rewind",
        ]),
        "rewind",
    );
    assert_failure_contains(
        &invoke(&["remote-input-media-seek", "127.0.0.1:9", "9", "1/0s"]),
        "1/0s",
    );
    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_eq!(
        stored
            .project()
            .media_playback(InputId::new(NonZeroU128::new(9).unwrap())),
        Some(&settings)
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn remote_audio_strip_preserves_controls_and_replicated_state() {
    let server = FakeRemoteServer::start_audio_strip();
    let output = invoke(&[
        "remote-audio-strip",
        &server.address(),
        "2",
        "-6000",
        "2500",
        "on",
        "on",
        "off",
        "2400",
        "--key",
        "remote-audio-strip",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    assert!(stdout(&output).contains(
        "AudioStrips=[1:\"Camera\":0:0:false:false:true:0,2:\"Slides\":-6000:2500:true:true:false:2400]"
    ));
    server.finish();
}

#[test]
fn remote_stinger_preserves_slot_duration_and_protocol() {
    let server = FakeRemoteServer::start_stinger();
    let output = invoke(&[
        "remote-stinger",
        &server.address(),
        "8",
        "3",
        "--key",
        "remote-stinger",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    server.finish();
}

#[test]
fn remote_stinger_configure_and_remove_preserve_current_protocol() {
    let server = FakeRemoteServer::start_stinger_configuration();
    let configured = invoke(&[
        "remote-stinger-configure",
        &server.address(),
        "3",
        "2",
        "true",
        "45",
        "mix-with-program",
        "fade",
        "--key",
        "configure-stinger",
        "--expect",
        "0",
    ]);
    assert_success(&configured);
    let removed = invoke(&[
        "remote-stinger-remove",
        &server.address(),
        "3",
        "--key",
        "remove-stinger",
        "--expect",
        "1",
    ]);
    assert_success(&removed);
    server.finish();
}

#[test]
fn remote_fade_to_black_preserves_target_duration_and_replicated_state() {
    let server = FakeRemoteServer::start_fade_to_black();
    let output = invoke(&[
        "remote-ftb",
        &server.address(),
        "black",
        "2",
        "--key",
        "remote-blackout",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    assert!(stdout(&output).contains("FTB(desired=black@0/65535, realized=black@65535/65535)"));
    server.finish();
}

#[test]
fn remote_t_bar_position_preserves_the_exact_endpoint_and_replicated_status() {
    let server = FakeRemoteServer::start_manual_position();
    let output = invoke(&[
        "remote-tbar-position",
        &server.address(),
        "10000",
        "--key",
        "manual-endpoint",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    assert!(stdout(&output).contains("TBar(desired=fade:1->2@10000, realized=fade:1->2@10000)"));
    server.finish();
}

#[test]
fn remote_commands_accept_peer_events_while_waiting() {
    let server = FakeRemoteServer::start_peer_event_interleave();
    let output = invoke_bounded(&[
        "remote-preview",
        &server.address(),
        "2",
        "--key",
        "peer-event-interleave",
    ]);
    assert_success(&output);
    assert_remote_status(&stdout(&output), 2, 2, 2, 2, 2);
    server.finish();
}

#[test]
fn ppm_publication_complete_deterministic_mvp_contract() {
    let context = ContractContext::new();
    let (project_id, created_status) = assert_contract_creation(&context);
    assert_initial_contract_state(&context, project_id, &created_status);
    assert_contract_render(&context, &context.first_image, 3, 2, [73, 151, 199]);

    select_contract_preview(&context);
    assert_contract_rejections(&context);
    let (after_cut, cut_manifest) = cut_contract(&context);
    assert_duplicate_cut_replay(&context, &after_cut, &cut_manifest);
    assert_contract_render(&context, &context.second_image, 2, 2, [146, 46, 142]);
    fade_and_assert_final_contract(&context, project_id);

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_wipe_preserves_duration_and_idempotency_contract() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let wipe = invoke(&[
        "wipe",
        context.project_path(),
        "3",
        "--key",
        "wipe-three",
        "--expect",
        "0",
    ]);
    assert_success(&wipe);
    let wipe_status = stdout(&wipe);
    assert_status(&wipe_status, 1, 3, 2, 2, 1, 1);
    let wipe_manifest = manifest(&context.project);

    let duplicate = invoke(&[
        "wipe",
        context.project_path(),
        "99",
        "--key",
        "wipe-three",
        "--expect",
        "0",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), wipe_status);
    assert_eq!(manifest(&context.project), wipe_manifest);

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_alpha_fade_settles_and_preserves_idempotency_contract() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let alpha_fade = invoke(&[
        "alpha-fade",
        context.project_path(),
        "3",
        "--key",
        "alpha-fade-three",
        "--expect",
        "0",
    ]);
    assert_success(&alpha_fade);
    let alpha_fade_status = stdout(&alpha_fade);
    assert_status(&alpha_fade_status, 1, 3, 2, 2, 1, 1);
    let alpha_fade_manifest = manifest(&context.project);

    let duplicate = invoke(&[
        "alpha-fade",
        context.project_path(),
        "99",
        "--key",
        "alpha-fade-three",
        "--expect",
        "0",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), alpha_fade_status);
    assert_eq!(manifest(&context.project), alpha_fade_manifest);

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_slide_settles_and_preserves_idempotency_contract() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let slide = invoke(&[
        "slide",
        context.project_path(),
        "3",
        "--key",
        "slide-three",
        "--expect",
        "0",
    ]);
    assert_success(&slide);
    let slide_status = stdout(&slide);
    assert_status(&slide_status, 1, 3, 2, 2, 1, 1);
    let slide_manifest = manifest(&context.project);

    let duplicate = invoke(&[
        "slide",
        context.project_path(),
        "99",
        "--key",
        "slide-three",
        "--expect",
        "0",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), slide_status);
    assert_eq!(manifest(&context.project), slide_manifest);

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_zoom_settles_and_preserves_idempotency_contract() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let zoom = invoke(&[
        "zoom",
        context.project_path(),
        "3",
        "--key",
        "zoom-three",
        "--expect",
        "0",
    ]);
    assert_success(&zoom);
    let zoom_status = stdout(&zoom);
    assert_status(&zoom_status, 1, 3, 2, 2, 1, 1);
    let zoom_manifest = manifest(&context.project);

    let duplicate = invoke(&[
        "zoom",
        context.project_path(),
        "99",
        "--key",
        "zoom-three",
        "--expect",
        "0",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), zoom_status);
    assert_eq!(manifest(&context.project), zoom_manifest);

    fs::remove_dir_all(context.root).unwrap();
}

/// FNV-1a 64 of a file's bytes, the golden form of a rendered PPM.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn transition_kinds_list_every_mix_and_overlay_transition() {
    let listed = invoke(&["transition-kinds"]);
    assert_success(&listed);
    let mixes = "fade alpha-fade wipe vertical-wipe slide vertical-slide zoom fly fly-rotate \
                 cube cube-zoom cross-zoom merge";
    let overlays = "cut fade wipe vertical-wipe slide vertical-slide zoom fly fly-rotate cube \
                    cube-zoom cross-zoom merge";
    let expected = mixes
        .split_whitespace()
        .map(|kind| format!("mix\t{kind}\n"))
        .chain(
            overlays
                .split_whitespace()
                .map(|kind| format!("overlay\t{kind}\n")),
        )
        .collect::<String>();
    assert_eq!(stdout(&listed), expected.trim_end());
}

#[test]
fn local_transition_by_kind_settles_and_preserves_idempotency_contract() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    for kind in [
        "vertical-wipe",
        "vertical-slide",
        "fly",
        "fly-rotate",
        "cube",
        "cube-zoom",
        "cross-zoom",
        "merge",
    ] {
        let taken = invoke(&["transition", context.project_path(), kind, "3"]);
        assert_success(&taken);
    }
    // Eight takes swap Program and Preview back to where they started.
    let cube = invoke(&[
        "transition",
        context.project_path(),
        "cube",
        "3",
        "--key",
        "cube-three",
        "--expect",
        "8",
    ]);
    assert_success(&cube);
    let cube_status = stdout(&cube);
    assert!(
        cube_status.contains(
            "revision=9 frame=27 Program(desired=2, realized=2) Preview(desired=1, realized=1)"
        ),
        "{cube_status}"
    );
    let manifest_after = manifest(&context.project);
    let duplicate = invoke(&[
        "transition",
        context.project_path(),
        "merge",
        "99",
        "--key",
        "cube-three",
        "--expect",
        "8",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), cube_status);
    assert_eq!(manifest(&context.project), manifest_after);
    assert_failure_contains(
        &invoke(&["transition", context.project_path(), "spin", "3"]),
        "spin",
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn transition_render_frame_matches_the_golden_for_every_new_kind() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let golden: [(&str, u64); 8] = TRANSITION_FRAME_GOLDENS;
    let mut measured = Vec::new();
    for (kind, _) in golden {
        let rendered = invoke(&[
            "transition-render-frame",
            context.project_path(),
            kind,
            "1",
            "3",
            context.final_image.to_str().unwrap(),
            "--width",
            "32",
            "--height",
            "18",
        ]);
        assert_success(&rendered);
        assert_eq!(
            stdout(&rendered),
            format!(
                "rendered 32x18 {kind} 1/3 from input 1 to input 2 to {}",
                context.final_image.display()
            )
        );
        measured.push((kind, fnv1a(&fs::read(&context.final_image).unwrap())));
    }
    assert_eq!(measured, golden, "{measured:#x?}");
    // The endpoints are exactly Program and Preview.
    for (numerator, image) in [("0", &context.first_image), ("3", &context.second_image)] {
        assert_success(&invoke(&[
            "transition-render-frame",
            context.project_path(),
            "cube",
            numerator,
            "3",
            image.to_str().unwrap(),
            "--width",
            "32",
            "--height",
            "18",
        ]));
    }
    assert_success(&invoke(&[
        "render",
        context.project_path(),
        context.final_image.to_str().unwrap(),
        "--width",
        "32",
        "--height",
        "18",
    ]));
    assert_eq!(
        fs::read(&context.first_image).unwrap(),
        fs::read(&context.final_image).unwrap()
    );
    assert_ne!(
        fs::read(&context.second_image).unwrap(),
        fs::read(&context.final_image).unwrap()
    );
    assert_failure_contains(
        &invoke(&[
            "transition-render-frame",
            context.project_path(),
            "cube",
            "4",
            "3",
            context.final_image.to_str().unwrap(),
        ]),
        "4/3",
    );
    fs::remove_dir_all(context.root).unwrap();
}

/// FNV-1a 64 of the 32x18 PPM of Program (bars) mixing to Preview at 1/3.
const TRANSITION_FRAME_GOLDENS: [(&str, u64); 8] = [
    ("vertical-wipe", 0x451e_8d41_018b_cd15),
    ("vertical-slide", 0xae8a_e3b9_4802_4d15),
    ("fly", 0xc5d4_e53f_c9fb_0015),
    ("fly-rotate", 0x3936_e77b_9e22_eccc),
    ("cube", 0x513a_fb16_b422_3bea),
    ("cube-zoom", 0x5af7_242e_28ac_2353),
    ("cross-zoom", 0x3c1e_fe3c_4b76_e255),
    ("merge", 0xbc1c_b3ba_5f06_9b39),
];

#[test]
fn transition_favourites_list_assign_persist_and_refuse_exactly() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let listed = invoke(&["transition-favorites", context.project_path()]);
    assert_success(&listed);
    assert_eq!(
        stdout(&listed),
        "1\tfade\t30\n2\twipe\t30\n3\tfly\t30\n4\tcube\t30"
    );
    assert_success(&invoke(&[
        "transition-favorite-set",
        context.project_path(),
        "2",
        "cross-zoom",
        "12",
        "--key",
        "favourite-two",
        "--expect",
        "0",
    ]));
    assert_success(&invoke(&[
        "transition-favorite-set",
        context.project_path(),
        "4",
        "stinger-3",
        "45",
    ]));
    assert_success(&invoke(&[
        "transition-favorite-set",
        context.project_path(),
        "1",
        "cut",
        "1",
    ]));
    let listed = invoke(&["transition-favorites", context.project_path()]);
    assert_eq!(
        stdout(&listed),
        "1\tcut\t1\n2\tcross-zoom\t12\n3\tfly\t30\n4\tstinger-3\t45"
    );
    let saved = fs::read_to_string(context.project.join("project.json")).unwrap();
    assert!(saved.contains(
        "\"transition_favorites\": [\n      {\"effect\": \"cut\", \"duration_frames\": 1},\n      \
         {\"effect\": \"cross_zoom\", \"duration_frames\": 12},\n      \
         {\"effect\": \"fly\", \"duration_frames\": 30},\n      \
         {\"effect\": \"stinger\", \"slot\": 3, \"duration_frames\": 45}\n    ]"
    ));
    let manifest_before = manifest(&context.project);
    for (slot, effect, frames, expected) in [
        ("5", "fade", "30", "favourite slot"),
        ("0", "fade", "30", "favourite slot"),
        ("1", "spin", "30", "spin"),
        ("1", "stinger-9", "30", "stinger-9"),
        ("1", "fade", "0", "favourite transition duration"),
        ("1", "fade", "3601", "favourite transition duration"),
    ] {
        assert_failure_contains(
            &invoke(&[
                "transition-favorite-set",
                context.project_path(),
                slot,
                effect,
                frames,
            ]),
            expected,
        );
    }
    assert_eq!(manifest(&context.project), manifest_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn overlay_transitions_accept_every_kind_and_show_it_in_status() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let configured = invoke(&[
        "overlay-transition",
        context.project_path(),
        "1",
        "fly-rotate",
        "5",
    ]);
    assert_success(&configured);
    assert!(stdout(&configured).contains("Overlays(desired=[1:none:off:opacity=0:fly-rotate@5:"));
    let taken = invoke(&["overlay-take", context.project_path(), "1", "2"]);
    assert_success(&taken);
    // A motion transition keeps the channel opaque and settles over its
    // duration.
    assert!(
        stdout(&taken).contains("Overlays(desired=[1:2:on:opacity=255:fly-rotate@5:"),
        "{}",
        stdout(&taken)
    );
    assert!(
        stdout(&taken).contains("realized=[1:2:on:opacity=255:fly-rotate@5:"),
        "{}",
        stdout(&taken)
    );
    assert_failure_contains(
        &invoke(&[
            "overlay-transition",
            context.project_path(),
            "1",
            "spin",
            "5",
        ]),
        "spin",
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_stinger_configure_rejects_unknown_input_without_persistence() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    let manifest_path = context.project.join("project.json");
    let manifest_before = fs::read(&manifest_path).unwrap();
    let journal_before = journal_bytes(&store);
    let unknown = invoke(&[
        "stinger-configure",
        context.project_path(),
        "8",
        "99",
        "true",
        "1",
        "muted",
        "cut",
    ]);
    assert_failure_contains(&unknown, "unknown stinger media input 99");
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(journal_bytes(&store), journal_before);

    let configured = invoke(&[
        "stinger-configure",
        context.project_path(),
        "8",
        "2",
        "true",
        "1",
        "muted",
        "cut",
    ]);
    assert_success(&configured);
    assert!(stdout(&configured).contains("Stingers=[8:2:preload:1:muted:cut]"));

    let stinger = invoke(&[
        "stinger",
        context.project_path(),
        "8",
        "3",
        "--key",
        "stinger-eight",
        "--expect",
        "0",
    ]);
    assert_success(&stinger);
    let stinger_status = stdout(&stinger);
    assert_status(&stinger_status, 1, 3, 2, 2, 1, 1);
    let stinger_manifest = manifest(&context.project);
    assert!(stinger_manifest.contains(r#""slot": 8"#));
    assert!(stinger_manifest.contains(r#""cut_point_frames": 1"#));

    let duplicate = invoke(&[
        "stinger",
        context.project_path(),
        "8",
        "99",
        "--key",
        "stinger-eight",
        "--expect",
        "0",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), stinger_status);
    assert_eq!(manifest(&context.project), stinger_manifest);

    let reconfigured = invoke(&[
        "stinger-configure",
        context.project_path(),
        "8",
        "1",
        "false",
        "9",
        "mix-with-program",
        "keep-program",
    ]);
    assert_success(&reconfigured);
    assert_status(&stdout(&reconfigured), 1, 3, 2, 2, 1, 1);
    assert!(
        stdout(&reconfigured).contains("Stingers=[8:1:deferred:9:mix-with-program:keep-program]")
    );
    let reconfigured_manifest = manifest(&context.project);
    assert_eq!(reconfigured_manifest.matches(r#""slot": 8"#).count(), 1);
    assert!(reconfigured_manifest.contains(r#""preload": false"#));
    assert!(reconfigured_manifest.contains(r#""cut_point_frames": 9"#));

    let removed = invoke(&["stinger-remove", context.project_path(), "8"]);
    assert_success(&removed);
    assert_status(&stdout(&removed), 1, 3, 2, 2, 1, 1);
    assert!(stdout(&removed).contains("Stingers=[]"));
    assert!(manifest(&context.project).contains(r#""stingers": []"#));

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_fade_to_black_settles_persists_and_reverses() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let black = invoke(&[
        "ftb",
        context.project_path(),
        "black",
        "3",
        "--key",
        "local-blackout",
        "--expect",
        "0",
    ]);
    assert_success(&black);
    assert_status(&stdout(&black), 1, 3, 1, 1, 2, 2);
    assert!(stdout(&black).contains("FTB(desired=black@65535/65535, realized=black@65535/65535)"));
    assert_eq!(status(&context.project), stdout(&black));
    let stored = manifest(&context.project);
    assert!(stored.contains(r#""desired": {"target_active": true, "position_numerator": 65535}"#));
    assert!(stored.contains(r#""realized": {"target_active": true, "position_numerator": 65535}"#));

    let repeated = invoke(&[
        "ftb",
        context.project_path(),
        "black",
        "99",
        "--key",
        "local-blackout-repeat",
        "--expect",
        "1",
    ]);
    assert_success(&repeated);
    assert_status(&stdout(&repeated), 2, 4, 1, 1, 2, 2);
    assert!(
        stdout(&repeated).contains("FTB(desired=black@65535/65535, realized=black@65535/65535)")
    );

    let live = invoke(&[
        "ftb",
        context.project_path(),
        "live",
        "2",
        "--key",
        "local-live",
        "--expect",
        "2",
    ]);
    assert_success(&live);
    assert_status(&stdout(&live), 3, 6, 1, 1, 2, 2);
    assert!(stdout(&live).contains("FTB(desired=live@0/65535, realized=live@0/65535)"));
    assert_eq!(status(&context.project), stdout(&live));

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_input_add_persists_default_simulated_strip() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let name = "Exact  input name  ";
    let added = invoke(&["input-add", context.project_path(), "3", name]);
    assert_success(&added);

    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let input = stored
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == InputId::new(NonZeroU128::new(3).unwrap()))
        .unwrap();
    assert_eq!(input.name, name);
    assert!(matches!(
        &input.kind,
        InputKind::Simulated(simulated)
            if simulated.video == SimulatedVideo::Bars
                && simulated.audio == SimulatedAudio::Silence
    ));
    assert_eq!(
        stored.project().input_audio_strip(input.id).unwrap(),
        InputAudioStripState::default()
    );

    assert_success(&invoke(&[
        "input-duplicate",
        context.project_path(),
        "3",
        "4",
        "Copied input",
    ]));
    let store = ProjectStore::new(&context.project).unwrap();
    let manifest_path = context.project.join("project.json");
    let before_failure = fs::read(&manifest_path).unwrap();
    let journal_before_failure = journal_bytes(&store);

    let duplicate = invoke(&["input-add", context.project_path(), "3", "Other"]);
    assert_failure_contains(&duplicate, "input 3 already exists");
    assert_eq!(fs::read(&manifest_path).unwrap(), before_failure);
    assert_eq!(journal_bytes(&store), journal_before_failure);

    let duplicate_name = invoke(&["input-duplicate", context.project_path(), "3", "5", name]);
    assert_failure_contains(&duplicate_name, "input name is already in use");
    assert_eq!(fs::read(&manifest_path).unwrap(), before_failure);
    assert_eq!(journal_bytes(&store), journal_before_failure);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_simulated_solid_input_add_persists_exact_rgba() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    let name = "Solid  input ";
    assert_success(&invoke(&[
        "simulated-solid-input-add",
        context.project_path(),
        "3",
        name,
        "1",
        "127",
        "254",
        "0",
    ]));

    let after = store.load().unwrap();
    let input = after
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == InputId::new(NonZeroU128::new(3).unwrap()))
        .unwrap();
    assert_eq!(input.name, name);
    assert_eq!(input.required_capabilities, Vec::<String>::new());
    assert!(matches!(
        &input.kind,
        InputKind::Simulated(simulated)
            if simulated.video == SimulatedVideo::Solid(SolidColor::new(1, 127, 254, 0))
                && simulated.audio == SimulatedAudio::Silence
    ));
    assert_eq!(
        after.project().input_audio_strip(input.id).unwrap(),
        InputAudioStripState::default()
    );
    assert_eq!(after.position(), before.position());
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    assert_success(&invoke(&["status", context.project_path()]));

    let manifest_before_failure = fs::read(context.project.join("project.json")).unwrap();
    let journal_before_failure = journal_bytes(&store);
    let invalid = invoke(&[
        "simulated-solid-input-add",
        context.project_path(),
        "4",
        "Invalid",
        "256",
        "0",
        "0",
        "255",
    ]);
    assert_failure_contains(&invalid, "red must be in 0..=255, got 256");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before_failure
    );
    assert_eq!(journal_bytes(&store), journal_before_failure);
    fs::remove_dir_all(context.root).unwrap();
}

/// A `.fmtitle` source document that needs no font: a rectangle tinted by a
/// color field, plus a number field.
const SHAPE_TITLE_SOURCE: &str = r##"{
  "schema_version": 36,
  "template": {
    "id": 7,
    "name": "Score bar",
    "width": 1920,
    "height": 1080,
    "background": "#00000000",
    "fields": [
      {"id": 2, "name": "Bar color", "default": {"type": "color", "value": "#112233FF"}},
      {"id": 1, "name": "Score", "default": {"type": "number", "value": -4}}
    ],
    "elements": [
      {"id": 1, "name": "Bar", "bounds": {"x": 0, "y": 900, "width": 1920, "height": 120}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": null, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": 2, "kind": {"type": "rectangle"}}
    ],
    "animation": {"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}
  },
  "fonts": [],
  "clocks": []
}
"##;

#[test]
#[allow(clippy::too_many_lines)]
fn title_input_add_imports_the_template_and_adds_default_fields() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    let source = context.root.join("score-bar.fmtitle");
    fs::write(&source, SHAPE_TITLE_SOURCE).unwrap();
    let source = source.to_str().unwrap();

    let added = invoke(&[
        "title-input-add",
        context.project_path(),
        "--id",
        "9",
        "--name",
        "Score bar",
        "--template",
        source,
    ]);
    assert_success(&added);
    assert!(
        stdout(&added).starts_with(
            "title-input-add: template asset://titles/score-bar.fmtitle copied bytes="
        ),
        "{}",
        stdout(&added)
    );
    let input = InputId::new(NonZeroU128::new(9).unwrap());
    let loaded = store.load().unwrap();
    let title = loaded
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert_eq!(title.name, "Score bar");
    let field = |id: u128, value| fm_model::TitleFieldEntry {
        id: fm_model::TitleFieldId::new(NonZeroU128::new(id).unwrap()),
        value,
    };
    assert_eq!(
        title.kind,
        InputKind::Title(fm_model::TitleInput {
            template_uri: "asset://titles/score-bar.fmtitle".into(),
            field_revision: 0,
            fields: vec![
                field(1, fm_model::TitleFieldValue::Number(-4)),
                field(
                    2,
                    fm_model::TitleFieldValue::Color(Rgba8::new(0x11, 0x22, 0x33, 0xff))
                ),
            ],
        })
    );
    assert_eq!(store.validate_titles(&loaded), Vec::new());
    assert!(
        context
            .project
            .join("assets/titles/score-bar.fmtitle")
            .is_file()
    );

    let inputs = invoke(&["inputs", context.project_path()]);
    assert_success(&inputs);
    assert!(
        stdout(&inputs).contains(
            "input id=9 name=\"Score bar\" kind=title template=asset://titles/score-bar.fmtitle field_revision=0 fields=2 strip="
        ),
        "{}",
        stdout(&inputs)
    );
    assert_success(&invoke(&["status", context.project_path()]));
    let audit = invoke(&["asset-audit", context.project_path()]);
    assert_success(&audit);
    assert_eq!(stdout(&audit), "asset-audit: clean");

    // A second title from the same document reuses the stored template.
    let again = invoke(&[
        "title-input-add",
        context.project_path(),
        "--id",
        "10",
        "--name",
        "Score bar 2",
        "--template",
        source,
    ]);
    assert_success(&again);
    assert!(
        stdout(&again)
            .starts_with("title-input-add: template asset://titles/score-bar.fmtitle reused")
    );

    // Refusals leave the manifest and assets untouched.
    let manifest = fs::read(context.project.join("project.json")).unwrap();
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "9",
            "--name",
            "Taken",
            "--template",
            source,
        ]),
        "input 9 already exists",
    );
    // A name the add would refuse is caught before the template is copied.
    let renamed = context.root.join("renamed-bar.fmtitle");
    fs::write(
        &renamed,
        SHAPE_TITLE_SOURCE.replace("\"id\": 7", "\"id\": 8"),
    )
    .unwrap();
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "11",
            "--name",
            "Score bar",
            "--template",
            renamed.to_str().unwrap(),
        ]),
        "input name is already in use",
    );
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "11",
            "--name",
            &"n".repeat(129),
            "--template",
            renamed.to_str().unwrap(),
        ]),
        "input name must not exceed 128 bytes",
    );
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "11",
            "--name",
            "Fonted",
            "--template",
            source,
            "--font",
            "Sans=/nonexistent/Sans.ttf",
        ]),
        "font family `Sans` is supplied but never used",
    );
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "11",
            "--name",
            "Fonted",
            "--template",
            source,
            "--font",
            "no-separator",
        ]),
        "expected Family=path",
    );
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "11",
            "--template",
            source,
        ]),
        "missing value for --name",
    );
    let stale = context.root.join("stale.fmtitle");
    fs::write(
        &stale,
        SHAPE_TITLE_SOURCE.replace("\"schema_version\": 36", "\"schema_version\": 35"),
    )
    .unwrap();
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "11",
            "--name",
            "Stale",
            "--template",
            stale.to_str().unwrap(),
        ]),
        "title template schema 35 is unsupported; expected 36",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_dir(context.project.join("assets/titles"))
            .unwrap()
            .count(),
        1
    );
    fs::remove_dir_all(context.root).unwrap();
}

/// A 16x8 font-free `.fmtitle` source: one opaque red 8x4 rectangle at
/// (4, 2) and no animation yet.
const SLIDE_TITLE_SOURCE: &str = r##"{
  "schema_version": 36,
  "template": {
    "id": 11,
    "name": "Slide",
    "width": 16,
    "height": 8,
    "background": "#00000000",
    "fields": [],
    "elements": [
      {"id": 1, "name": "Bar", "bounds": {"x": 4, "y": 2, "width": 8, "height": 4}, "z_index": 0, "visible": true, "style": {"fill": "#ff0000ff", "background": null, "opacity": 255, "font": null, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "rectangle"}}
    ],
    "animation": {"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}
  },
  "fonts": [],
  "clocks": []
}
"##;

/// The binary PPM golden of the 16x8 slide title: `color` over columns
/// `columns` of rows 2..6, black elsewhere (premultiplied over black).
fn slide_golden(columns: std::ops::Range<u32>, color: [u8; 3]) -> Vec<u8> {
    let mut golden = b"P6\n16 8\n255\n".to_vec();
    for y in 0..8 {
        for x in 0..16 {
            let inside = (2..6).contains(&y) && columns.contains(&x);
            golden.extend_from_slice(&if inside { color } else { [0; 3] });
        }
    }
    golden
}

#[test]
#[allow(clippy::too_many_lines)]
fn title_animation_presets_retime_and_render_exact_frames() {
    const RED: [u8; 3] = [255, 0, 0];
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let source = context.root.join("slide.fmtitle");
    fs::write(&source, SLIDE_TITLE_SOURCE).unwrap();
    assert_success(&invoke(&[
        "title-input-add",
        context.project_path(),
        "--id",
        "9",
        "--name",
        "Slide",
        "--template",
        source.to_str().unwrap(),
    ]));
    let uri = "asset://titles/slide.fmtitle";
    let render = |phase: &str, frame: &str| {
        let output = context.root.join(format!("{phase}-{frame}.ppm"));
        let rendered = invoke(&[
            "title-render-frame",
            context.project_path(),
            "9",
            "--phase",
            phase,
            "--frame",
            frame,
            output.to_str().unwrap(),
        ]);
        assert_success(&rendered);
        assert!(
            stdout(&rendered).starts_with(&format!(
                "title-render-frame: input 9 phase={phase} frame={frame} 16x8 to "
            )),
            "{}",
            stdout(&rendered)
        );
        fs::read(output).unwrap()
    };
    // Without animation every frame is the static design.
    assert_eq!(render("in", "0"), slide_golden(4..12, RED));

    let preset = invoke(&[
        "title-animation-preset",
        context.project_path(),
        uri,
        "1",
        "--phase",
        "in",
        "--preset",
        "slide-from-left",
        "--frames",
        "10",
    ]);
    assert_success(&preset);
    assert_eq!(
        stdout(&preset),
        "title-animation-preset: asset://titles/slide.fmtitle revision=1 in_frames=10 out_frames=0 tracks=1"
    );
    let stored = fs::read_to_string(context.project.join("assets/titles/slide.fmtitle")).unwrap();
    assert!(stored.contains(
        "{\"element\": 1, \"phase\": \"in\", \"property\": \"position_x\", \"keys\": [{\"frame\": 0, \"value\": -12, \"easing\": \"cubic-out\"}, {\"frame\": 10, \"value\": 0, \"easing\": \"linear\"}]}"
    ), "{stored}");

    // Frame 0 starts fully off the left edge (x = 4 - 12); frame N is the
    // static design; halfway CubicOut is 0.875: -12 + trunc(10.5) = -2.
    assert_eq!(render("in", "0"), slide_golden(0..0, RED));
    assert_eq!(render("in", "10"), slide_golden(4..12, RED));
    assert_eq!(render("in", "5"), slide_golden(2..10, RED));

    // An Out fade: halfway through four frames is 255 - trunc(127.5).
    assert_success(&invoke(&[
        "title-animation-preset",
        context.project_path(),
        uri,
        "1",
        "--phase",
        "out",
        "--preset",
        "fade",
        "--frames",
        "4",
    ]));
    assert_eq!(render("out", "0"), slide_golden(4..12, RED));
    assert_eq!(render("out", "2"), slide_golden(4..12, [128, 0, 0]));
    assert_eq!(render("out", "4"), slide_golden(0..0, RED));

    // Stretching the In phase to 20 frames moves the halfway pose to 10.
    let stretched = invoke(&[
        "title-animation-stretch",
        context.project_path(),
        uri,
        "--phase",
        "in",
        "--frames",
        "20",
    ]);
    assert_success(&stretched);
    assert_eq!(
        stdout(&stretched),
        "title-animation-stretch: asset://titles/slide.fmtitle revision=3 in_frames=20 out_frames=4 tracks=2"
    );
    assert_eq!(render("in", "10"), slide_golden(2..10, RED));
    assert_eq!(render("in", "20"), slide_golden(4..12, RED));

    // The stretched In phase saved as a user preset (GX-011) and applied
    // back over 10 frames reproduces the original slide exactly.
    let saved = invoke(&[
        "preset-save",
        context.project_path(),
        uri,
        "1",
        "--phase",
        "in",
        "--name",
        "Slide in",
    ]);
    assert_success(&saved);
    assert!(
        stdout(&saved).starts_with("preset-save: preset 1 \"Slide in\" phase=in frames=20"),
        "{}",
        stdout(&saved)
    );
    assert_success(&invoke(&[
        "title-animation-preset",
        context.project_path(),
        uri,
        "1",
        "--phase",
        "in",
        "--preset",
        "user:1",
        "--frames",
        "10",
    ]));
    assert_eq!(render("in", "5"), slide_golden(2..10, RED));
    assert_eq!(render("in", "10"), slide_golden(4..12, RED));
    assert_failure_contains(
        &invoke(&[
            "preset-save",
            context.project_path(),
            uri,
            "7",
            "--phase",
            "in",
            "--name",
            "Nothing",
        ]),
        "element 7",
    );
    assert_success(&invoke(&[
        "title-animation-stretch",
        context.project_path(),
        uri,
        "--phase",
        "in",
        "--frames",
        "20",
    ]));

    // Refusals leave the stored template untouched.
    let before = fs::read(context.project.join("assets/titles/slide.fmtitle")).unwrap();
    for (arguments, expected) in [
        (
            vec!["1", "--phase", "in", "--preset", "spin", "--frames", "4"],
            "unknown animation preset `spin`",
        ),
        (
            vec!["7", "--phase", "in", "--preset", "fade", "--frames", "4"],
            "unknown title element 7",
        ),
        (
            vec![
                "1", "--phase", "in", "--preset", "fade", "--frames", "36001",
            ],
            "exceeds 36000",
        ),
    ] {
        let mut command = vec!["title-animation-preset", context.project_path(), uri];
        command.extend(arguments);
        assert_failure_contains(&invoke(&command), expected);
    }
    assert_failure_contains(
        &invoke(&[
            "title-animation-stretch",
            context.project_path(),
            "asset://titles/missing.fmtitle",
            "--phase",
            "in",
            "--frames",
            "4",
        ]),
        "title template asset://titles/missing.fmtitle",
    );
    assert_failure_contains(
        &invoke(&[
            "title-animation-stretch",
            context.project_path(),
            uri,
            "--phase",
            "hold",
            "--frames",
            "4",
        ]),
        "phase (expected in or out)",
    );
    assert_eq!(
        fs::read(context.project.join("assets/titles/slide.fmtitle")).unwrap(),
        before
    );
    assert_failure_contains(
        &invoke(&[
            "title-render-frame",
            context.project_path(),
            "77",
            "--phase",
            "in",
            "--frame",
            "0",
            context.root.join("never.ppm").to_str().unwrap(),
        ]),
        "input 77 is not a title input",
    );
    fs::remove_dir_all(context.root).unwrap();
}

/// A `.fmtitle` source document whose three production clock elements show
/// a countdown to doors, US Eastern time of day, and a since-take stopwatch,
/// all drawn with the `Clock Face` font family.
const CLOCK_TITLE_SOURCE: &str = r##"{
  "schema_version": 36,
  "template": {
    "id": 9,
    "name": "Clocks",
    "width": 1920,
    "height": 1080,
    "background": "#00000000",
    "fields": [],
    "elements": [
      {"id": 1, "name": "Doors", "bounds": {"x": 0, "y": 0, "width": 600, "height": 80}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": {"family": "Clock Face", "size_px": 48}, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "production_clock", "clock": "doors", "format": null}},
      {"id": 2, "name": "Local", "bounds": {"x": 0, "y": 100, "width": 600, "height": 80}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": {"family": "Clock Face", "size_px": 48}, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "production_clock", "clock": "eastern", "format": {"type": "time_of_day", "preset": "hours_12"}}},
      {"id": 3, "name": "On air", "bounds": {"x": 0, "y": 200, "width": 600, "height": 80}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": {"family": "Clock Face", "size_px": 48}, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "production_clock", "clock": "on-air", "format": null}}
    ],
    "animation": {"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}
  },
  "fonts": [],
  "clocks": [
    {"name": "doors", "kind": "countdown_to", "target_utc": "2026-10-14T23:00:00Z", "overrun": "continue", "format": {"type": "duration", "preset": "minutes_seconds"}},
    {"name": "eastern", "kind": "time_of_day", "zone": "EST5EDT,M3.2.0,M11.1.0", "format": {"type": "time_of_day", "preset": "hours_24"}},
    {"name": "on-air", "kind": "since_take", "format": {"type": "duration", "preset": "hours_minutes_seconds_frames", "frame_rate": {"numerator": 30000, "denominator": 1001}}}
  ]
}
"##;

/// A usable system font file: `FM_TITLES_FONT`, then well-known paths, then
/// the first `.ttf`/`.otf` under the system font roots. `None` (and the
/// caller skips) unless `FM_REQUIRE_FONT=1`.
fn discover_font_file() -> Option<PathBuf> {
    fn scan(directory: &Path, depth: u32) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        let mut entries = fs::read_dir(directory)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        entries.sort();
        entries
            .iter()
            .find(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("ttf")
                                || extension.eq_ignore_ascii_case("otf")
                        })
            })
            .cloned()
            .or_else(|| {
                entries
                    .iter()
                    .filter(|path| path.is_dir())
                    .find_map(|path| scan(path, depth + 1))
            })
    }
    let found = std::env::var_os("FM_TITLES_FONT")
        .map(PathBuf::from)
        .into_iter()
        .chain(
            [
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "C:\\Windows\\Fonts\\arial.ttf",
            ]
            .into_iter()
            .map(PathBuf::from),
        )
        .find(|path| path.is_file())
        .or_else(|| {
            [
                "/usr/share/fonts",
                "/usr/local/share/fonts",
                "/Library/Fonts",
                "/System/Library/Fonts",
                "C:\\Windows\\Fonts",
            ]
            .into_iter()
            .find_map(|root| scan(Path::new(root), 0))
        });
    if found.is_none() {
        assert!(
            std::env::var("FM_REQUIRE_FONT").as_deref() != Ok("1"),
            "FM_REQUIRE_FONT=1 but no usable font file was found"
        );
        eprintln!("skipping clock title import: no font file available");
    }
    found
}

#[test]
#[allow(clippy::too_many_lines)]
fn title_input_add_imports_a_production_clock_template_with_its_clock_table() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    let manifest = fs::read(context.project.join("project.json")).unwrap();

    // The clock table must define exactly the clocks the elements show; this
    // is checked before the font is read, so it needs no real font.
    let undefined = context.root.join("undefined.fmtitle");
    fs::write(
        &undefined,
        CLOCK_TITLE_SOURCE.replace("\"clock\": \"on-air\"", "\"clock\": \"later\""),
    )
    .unwrap();
    let not_a_font = context.root.join("not-a-font.ttf");
    fs::write(&not_a_font, b"not a font").unwrap();
    let font_argument = |path: &Path| format!("Clock Face={}", path.to_str().unwrap());
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "9",
            "--name",
            "Clocks",
            "--template",
            undefined.to_str().unwrap(),
            "--font",
            &font_argument(&not_a_font),
        ]),
        "title template clocks: element 3 shows clock later, which is not defined",
    );
    let bad_zone = context.root.join("bad-zone.fmtitle");
    fs::write(
        &bad_zone,
        CLOCK_TITLE_SOURCE.replace("EST5EDT,M3.2.0,M11.1.0", "America/New_York"),
    )
    .unwrap();
    assert_failure_contains(
        &invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            "9",
            "--name",
            "Clocks",
            "--template",
            bad_zone.to_str().unwrap(),
            "--font",
            &font_argument(&not_a_font),
        ]),
        "malformed title template",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest
    );
    assert!(!context.project.join("assets/titles").exists());

    let Some(font) = discover_font_file() else {
        fs::remove_dir_all(context.root).unwrap();
        return;
    };
    let source = context.root.join("clocks.fmtitle");
    fs::write(&source, CLOCK_TITLE_SOURCE).unwrap();
    let added = invoke(&[
        "title-input-add",
        context.project_path(),
        "--id",
        "9",
        "--name",
        "Clocks",
        "--template",
        source.to_str().unwrap(),
        "--font",
        &font_argument(&font),
    ]);
    assert_success(&added);
    assert!(
        stdout(&added)
            .starts_with("title-input-add: template asset://titles/clocks.fmtitle copied bytes="),
        "{}",
        stdout(&added)
    );
    // The stored template keeps the clock table and element presets exactly.
    let stored = fs::read_to_string(context.project.join("assets/titles/clocks.fmtitle")).unwrap();
    for line in CLOCK_TITLE_SOURCE
        .lines()
        .filter(|line| line.contains("\"kind\": \"") && line.trim_start().starts_with("{\"name\""))
    {
        assert!(
            stored.contains(line.trim_end_matches(',')),
            "missing {line} in {stored}"
        );
    }
    assert!(stored.contains(
        "\"kind\": {\"type\": \"production_clock\", \"clock\": \"eastern\", \"format\": {\"type\": \"time_of_day\", \"preset\": \"hours_12\"}}"
    ));
    let template = store
        .load_title_template("asset://titles/clocks.fmtitle")
        .unwrap();
    assert_eq!(
        template
            .clocks
            .iter()
            .map(|clock| clock.name.as_str())
            .collect::<Vec<_>>(),
        vec!["doors", "eastern", "on-air"]
    );
    let loaded = store.load().unwrap();
    let title = loaded
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == InputId::new(NonZeroU128::new(9).unwrap()))
        .unwrap();
    assert_eq!(
        title.kind,
        InputKind::Title(fm_model::TitleInput {
            template_uri: "asset://titles/clocks.fmtitle".into(),
            field_revision: 0,
            fields: Vec::new(),
        })
    );
    assert_eq!(store.validate_titles(&loaded), Vec::new());
    let audit = invoke(&["asset-audit", context.project_path()]);
    assert_success(&audit);
    assert_eq!(stdout(&audit), "asset-audit: clean");
    fs::remove_dir_all(context.root).unwrap();
}

/// A 16x8 `.fmtitle` source with one image element at (4, 2) sized 8x4
/// that stretches image field 1 nearest-neighbor; its default names
/// `asset://images/logo.png`.
const IMAGE_TITLE_SOURCE: &str = r##"{
  "schema_version": 36,
  "template": {
    "id": 12,
    "name": "Logo",
    "width": 16,
    "height": 8,
    "background": "#00000000",
    "fields": [
      {"id": 1, "name": "Logo", "default": {"type": "image", "asset_uri": "asset://images/logo.png"}}
    ],
    "elements": [
      {"id": 1, "name": "Logo", "bounds": {"x": 4, "y": 2, "width": 8, "height": 4}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": null, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "image", "field": 1, "fit": "stretch", "filter": "nearest"}}
    ],
    "animation": {"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}
  },
  "fonts": [],
  "clocks": []
}
"##;

/// GX-011: `title-render-frame` decodes image assets exactly as `freemixd`
/// does (the shared `fm-title-render` path): a 2x1 PNG stretched into the
/// element is drawn pixel-exact, and a missing image keeps the placeholder
/// and is reported.
#[test]
fn title_render_frame_draws_decoded_images() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    // A red-then-blue 2x1 PNG, stretched over 8x4 at (4, 2).
    let images = context.project.join("assets/images");
    fs::create_dir_all(&images).unwrap();
    let mut png = image::RgbaImage::new(2, 1);
    png.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    png.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
    png.save(images.join("logo.png")).unwrap();
    let source = context.root.join("logo.fmtitle");
    fs::write(&source, IMAGE_TITLE_SOURCE).unwrap();
    assert_success(&invoke(&[
        "title-input-add",
        context.project_path(),
        "--id",
        "9",
        "--name",
        "Logo",
        "--template",
        source.to_str().unwrap(),
    ]));
    let render = |name: &str| {
        let output = context.root.join(name);
        let rendered = invoke(&[
            "title-render-frame",
            context.project_path(),
            "9",
            "--phase",
            "in",
            "--frame",
            "0",
            output.to_str().unwrap(),
        ]);
        assert_success(&rendered);
        (stdout(&rendered), fs::read(output).unwrap())
    };
    let (report, drawn) = render("logo.ppm");
    assert!(!report.contains("placeholder image"), "{report}");
    assert_eq!(drawn, image_golden());

    // An image that no longer resolves keeps the placeholder and is named.
    fs::remove_file(images.join("logo.png")).unwrap();
    let (report, placeholder) = render("missing.ppm");
    assert!(
        report.contains("placeholder image: field 1 asset://images/logo.png"),
        "{report}"
    );
    assert_ne!(placeholder, image_golden());
    fs::remove_dir_all(context.root).unwrap();
}

/// The binary PPM golden of the logo title: red over columns 4..8 and blue
/// over 8..12 of rows 2..6, black elsewhere.
fn image_golden() -> Vec<u8> {
    let mut golden = b"P6\n16 8\n255\n".to_vec();
    for y in 0..8 {
        for x in 0..16 {
            golden.extend_from_slice(&match (x, y) {
                (4..8, 2..6) => [255, 0, 0],
                (8..12, 2..6) => [0, 0, 255],
                _ => [0; 3],
            });
        }
    }
    golden
}

/// A 200x80 `.fmtitle` source with the countdown `doors` (23:00 UTC) at
/// (0, 0) and the since-take stopwatch `on-air` at (0, 40), both
/// `minutes_seconds` in the `Clock Face` family, and a 120-frame In phase.
const CLOCK_RENDER_SOURCE: &str = r##"{
  "schema_version": 36,
  "template": {
    "id": 13,
    "name": "Clock render",
    "width": 200,
    "height": 80,
    "background": "#000000ff",
    "fields": [],
    "elements": [
      {"id": 1, "name": "Doors", "bounds": {"x": 0, "y": 0, "width": 200, "height": 40}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": {"family": "Clock Face", "size_px": 28}, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "production_clock", "clock": "doors", "format": null}},
      {"id": 2, "name": "On air", "bounds": {"x": 0, "y": 40, "width": 200, "height": 40}, "z_index": 0, "visible": true, "style": {"fill": "#ffffffff", "background": null, "opacity": 255, "font": {"family": "Clock Face", "size_px": 28}, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "production_clock", "clock": "on-air", "format": null}}
    ],
    "animation": {"timebase": "30/1", "revision": 0, "in_frames": 120, "out_frames": 0, "tracks": []}
  },
  "fonts": [],
  "clocks": [
    {"name": "doors", "kind": "countdown_to", "target_utc": "2026-10-14T23:00:00Z", "overrun": "continue", "format": {"type": "duration", "preset": "minutes_seconds"}},
    {"name": "on-air", "kind": "since_take", "format": {"type": "duration", "preset": "minutes_seconds"}}
  ]
}
"##;

/// The oracle of [`CLOCK_RENDER_SOURCE`]: the same layout with plain text
/// elements showing `doors` and `on_air`.
fn clock_oracle_source(doors: &str, on_air: &str) -> String {
    CLOCK_RENDER_SOURCE
        .replace("\"id\": 13", "\"id\": 14")
        .replace(
            "\"fields\": []",
            &format!(
                "\"fields\": [{{\"id\": 1, \"name\": \"Doors\", \"default\": {{\"type\": \"text\", \"value\": \"{doors}\"}}}}, {{\"id\": 2, \"name\": \"On air\", \"default\": {{\"type\": \"text\", \"value\": \"{on_air}\"}}}}]"
            ),
        )
        .replace(
            "{\"type\": \"production_clock\", \"clock\": \"doors\", \"format\": null}",
            "{\"type\": \"text\", \"field\": 1}",
        )
        .replace(
            "{\"type\": \"production_clock\", \"clock\": \"on-air\", \"format\": null}",
            "{\"type\": \"text\", \"field\": 2}",
        )
        .replace(
            &CLOCK_RENDER_SOURCE[CLOCK_RENDER_SOURCE.find("\"clocks\": [").unwrap()
                ..=CLOCK_RENDER_SOURCE.rfind(']').unwrap()],
            "\"clocks\": []",
        )
}

/// GX-011: `title-render-frame` draws production clocks exactly as
/// `freemixd` does, at a deterministic time: countdowns read `--at`, and
/// since-take clocks the clip position after a take at the first In frame
/// (Out after the whole In phase). Each render must equal a plain text
/// title holding the expected text (needs a real font: skips unless one is
/// found or `FM_REQUIRE_FONT=1`).
#[test]
fn title_render_frame_draws_production_clocks_at_a_deterministic_time() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let render = |input: &str, phase: &str, frame: &str, at: Option<&str>| {
        let output = context.root.join(format!("{input}-{phase}-{frame}.ppm"));
        let mut arguments = vec![
            "title-render-frame",
            context.project_path(),
            input,
            "--phase",
            phase,
            "--frame",
            frame,
        ];
        if let Some(at) = at {
            arguments.extend(["--at", at]);
        }
        let output_argument = output.to_str().unwrap().to_owned();
        arguments.push(&output_argument);
        let rendered = invoke(&arguments);
        assert_success(&rendered);
        fs::read(output).unwrap()
    };
    let Some(font) = discover_font_file() else {
        fs::remove_dir_all(context.root).unwrap();
        return;
    };
    let font_argument = format!("Clock Face={}", font.to_str().unwrap());
    let add = |id: &str, name: &str, source: &str| {
        let path = context.root.join(format!("{name}.fmtitle"));
        fs::write(&path, source).unwrap();
        assert_success(&invoke(&[
            "title-input-add",
            context.project_path(),
            "--id",
            id,
            "--name",
            name,
            "--template",
            path.to_str().unwrap(),
            "--font",
            &font_argument,
        ]));
    };
    add("9", "clocks", CLOCK_RENDER_SOURCE);
    for (index, (doors, on_air)) in [("00:30", "00:03"), ("-01:05", "00:05"), ("00:00", "00:00")]
        .into_iter()
        .enumerate()
    {
        add(
            &(20 + index).to_string(),
            &format!("oracle-{index}"),
            &clock_oracle_source(doors, on_air),
        );
    }
    let oracle = |index: usize| render(&(20 + index).to_string(), "in", "0", None);
    assert_ne!(oracle(0), oracle(1), "the font draws the clock text");

    // 30 s before the doors, 90 frames (3 s at 30/1) after the take.
    assert_eq!(
        render("9", "in", "90", Some("2026-10-14T22:59:30Z")),
        oracle(0)
    );
    // Out frame 30 is 120 + 30 frames (5 s) after the take; 65 s past the
    // doors counts negative.
    assert_eq!(
        render("9", "out", "30", Some("2026-10-14T23:01:05Z")),
        oracle(1)
    );
    // At the target the countdown shows zero; the take frame shows zero.
    assert_eq!(
        render("9", "in", "0", Some("2026-10-14T23:00:00Z")),
        oracle(2)
    );
    assert_failure_contains(
        &invoke(&[
            "title-render-frame",
            context.project_path(),
            "9",
            "--phase",
            "in",
            "--frame",
            "0",
            "--at",
            "2026-10-14 23:00:00",
            context.root.join("never.ppm").to_str().unwrap(),
        ]),
        "--at `2026-10-14 23:00:00` is not a YYYY-MM-DDTHH:MM:SSZ UTC time",
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_media_input_add_persists_offline_asset_contract() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    assert_success(&invoke(&[
        "media-input-add",
        context.project_path(),
        "9",
        "Media  input ",
        "asset://missing/video.ppm",
    ]));
    let after = store.load().unwrap();
    let input = after
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == InputId::new(NonZeroU128::new(9).unwrap()))
        .unwrap();
    assert_eq!(input.name, "Media  input ");
    assert!(matches!(
        &input.kind,
        InputKind::Media { asset_uri, .. } if asset_uri == "asset://missing/video.ppm"
    ));
    assert_eq!(input.required_capabilities.len(), 0);
    assert_eq!(
        after.project().input_audio_strip(input.id).unwrap(),
        InputAudioStripState::default()
    );
    assert_eq!(after.position(), before.position());
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());

    let audit = invoke(&["asset-audit", context.project_path()]);
    assert!(!audit.status.success());
    assert_eq!(stdout(&audit), "input=9 reason=missing-asset");

    let manifest_before_failure = fs::read(context.project.join("project.json")).unwrap();
    let journal_before_failure = journal_bytes(&store);
    let invalid = invoke(&[
        "media-input-add",
        context.project_path(),
        "10",
        "Invalid",
        "not-an-asset",
    ]);
    assert_failure_contains(&invalid, "invalid project asset URI");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before_failure
    );
    assert_eq!(journal_bytes(&store), journal_before_failure);

    let duplicate_id = invoke(&[
        "media-input-add",
        context.project_path(),
        "9",
        "Other",
        "asset://other.ppm",
    ]);
    assert_failure_contains(&duplicate_id, "input 9 already exists");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before_failure
    );
    assert_eq!(journal_bytes(&store), journal_before_failure);
    let duplicate_name = invoke(&[
        "media-input-add",
        context.project_path(),
        "10",
        "Media  input ",
        "asset://other.ppm",
    ]);
    assert_failure_contains(&duplicate_name, "input name is already in use");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before_failure
    );
    assert_eq!(journal_bytes(&store), journal_before_failure);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_media_input_relink_preserves_identity_and_runtime() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "media-input-add",
        context.project_path(),
        "9",
        "Media  input ",
        "asset://missing/old.ppm",
    ]));
    assert_success(&invoke(&[
        "input-add",
        context.project_path(),
        "3",
        "Other",
    ]));
    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    let before_input = before
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == InputId::new(NonZeroU128::new(9).unwrap()))
        .unwrap()
        .clone();
    let before_status = invoke(&["status", context.project_path()]);
    assert_success(&before_status);

    let relinked = invoke(&[
        "media-input-relink",
        context.project_path(),
        "9",
        "asset://missing/new.ppm",
    ]);
    assert_success(&relinked);
    let after = store.load().unwrap();
    let after_input = after
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == before_input.id)
        .unwrap();
    assert_eq!(after_input.id, before_input.id);
    assert_eq!(after_input.name, before_input.name);
    assert_eq!(
        after_input.required_capabilities,
        before_input.required_capabilities
    );
    assert!(matches!(
        &after_input.kind,
        InputKind::Media { asset_uri, .. } if asset_uri == "asset://missing/new.ppm"
    ));
    assert_eq!(
        after
            .project()
            .inputs()
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        before
            .project()
            .inputs()
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().input_audio_strip(before_input.id),
        before.project().input_audio_strip(before_input.id)
    );
    assert_eq!(after.position(), before.position());
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let status = invoke(&["status", context.project_path()]);
    assert_success(&status);
    assert_eq!(stdout(&status), stdout(&before_status));
    let audit = invoke(&["asset-audit", context.project_path()]);
    assert!(!audit.status.success());
    assert_eq!(stdout(&audit), "input=9 reason=missing-asset");

    for (command, message) in [
        (
            vec![
                "media-input-relink",
                context.project_path(),
                "9",
                "not-an-asset",
            ],
            "invalid project asset URI",
        ),
        (
            vec![
                "media-input-relink",
                context.project_path(),
                "99",
                "asset://missing/x.ppm",
            ],
            "unknown input",
        ),
        (
            vec![
                "media-input-relink",
                context.project_path(),
                "3",
                "asset://missing/x.ppm",
            ],
            "not a media input",
        ),
    ] {
        let manifest = fs::read(context.project.join("project.json")).unwrap();
        let journal = journal_bytes(&store);
        let failure = invoke(&command);
        assert_failure_contains(&failure, message);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest
        );
        assert_eq!(journal_bytes(&store), journal);
    }
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_asset_audit_reports_stable_issues_without_mutation() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    let initial = store.load().unwrap();
    let mut project = initial.project().clone();
    project.add_input(fm_model::Input {
        id: InputId::new(NonZeroU128::new(20).unwrap()),
        name: "Missing".into(),
        kind: InputKind::Media {
            asset_uri: "asset://missing.ppm".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(fm_model::Input {
        id: InputId::new(NonZeroU128::new(10).unwrap()),
        name: "Invalid".into(),
        kind: InputKind::Media {
            asset_uri: "not-an-asset".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    let stored = StoredProject::from_project_with_complete_runtime_state(
        project,
        initial.runtime_routing(),
        initial.runtime_manual_transitions(),
        initial.runtime_fade_to_black(),
        initial.runtime_overlays().clone(),
        initial.position(),
        initial.idempotency_receipts().to_vec(),
    )
    .unwrap();
    store.save(&stored).unwrap();
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);

    let issues = invoke(&["asset-audit", context.project_path()]);
    assert!(!issues.status.success());
    assert_eq!(
        String::from_utf8(issues.stdout.clone()).unwrap(),
        "input=10 reason=invalid-uri\ninput=20 reason=missing-asset\n"
    );
    let output = String::from_utf8_lossy(&issues.stdout);
    assert!(!output.contains("asset://"));
    assert!(!output.contains(context.project_path()));
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);

    fs::create_dir_all(store.assets_root()).unwrap();
    fs::write(store.assets_root().join("missing.ppm"), b"tiny").unwrap();
    let mut fixed = store.load().unwrap().project().clone();
    fixed
        .replace_input_source(
            InputId::new(NonZeroU128::new(10).unwrap()),
            InputKind::Media {
                asset_uri: "asset://missing.ppm".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            Vec::new(),
        )
        .unwrap();
    let fixed = StoredProject::from_project_with_complete_runtime_state(
        fixed,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )
    .unwrap();
    store.save(&fixed).unwrap();
    let clean = invoke(&["asset-audit", context.project_path()]);
    assert_success(&clean);
    assert_eq!(stdout(&clean), "asset-audit: clean");
    fs::remove_dir_all(context.root).unwrap();
}

fn journal_bytes(store: &ProjectStore) -> Vec<(PathBuf, Vec<u8>)> {
    fs::read_dir(store.journal_path())
        .ok()
        .into_iter()
        .flatten()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect()
}

const STREAM_KEY: &str = "live-7c2a-secret-key";

/// Authors a project with one output and one streaming destination on it.
fn author_stream_destination(context: &ContractContext) {
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    assert_success(&invoke_bounded(&[
        "scene-input-add",
        context.project_path(),
        "10",
        "10",
        "Program",
    ]));
    assert_success(&invoke_bounded(&[
        "audio-bus-add",
        context.project_path(),
        "20",
        "Master",
    ]));
    assert_success(&invoke_bounded(&[
        "output-add",
        context.project_path(),
        "30",
        "10",
        "20",
        "Primary",
    ]));
    assert_success(&invoke_bounded(&[
        "stream-add",
        context.project_path(),
        "40",
        "30",
        "rtmps://ingest.example.test:443/live",
        STREAM_KEY,
        "Main ingest",
        "--backup",
        "rtmps://backup.example.test/live",
        "--startup",
        "reconcile-desired-state",
    ]));
}

/// Every authoring refusal must be reported and must leave the bundle
/// byte-identical, including the one that protects a published output.
#[allow(clippy::too_many_lines)]
fn assert_stream_authoring_refusals_are_atomic(context: &ContractContext) {
    let unchanged = fs::read(context.project.join("project.json")).unwrap();
    for (arguments, expected) in [
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "rtmps://ingest.example.test/live",
                STREAM_KEY,
                "mAiN iNgEsT",
            ],
            "duplicate stream destination name",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "40",
                "30",
                "rtmps://ingest.example.test/live",
                STREAM_KEY,
                "Second",
            ],
            "duplicate stream destination 40",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "999",
                "rtmps://ingest.example.test/live",
                STREAM_KEY,
                "Second",
            ],
            "unknown output 999",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "rtmps://operator:hunter2@ingest.example.test/live",
                STREAM_KEY,
                "Second",
            ],
            "must not embed user:password credentials",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "rtmps://ingest.example.test/live",
                STREAM_KEY,
                "Second",
                "--video-bitrate-kbps",
                "999",
            ],
            "video bitrate must be at least 1000 kbps",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "rtmps://ingest.example.test/live",
                STREAM_KEY,
                "Second",
                "--video-bitrate-kbps",
                "100001",
            ],
            "video bitrate must not exceed 100000 kbps",
        ),
        (
            vec![
                "stream-update",
                context.project_path(),
                "999",
                "30",
                "rtmps://ingest.example.test/live",
                STREAM_KEY,
                "Missing",
            ],
            "unknown stream destination 999",
        ),
        (
            vec!["output-remove", context.project_path(), "30"],
            "output 30 is used by stream destination 40",
        ),
    ] {
        let rejected = invoke_bounded(&arguments);
        assert_failure_contains(&rejected, expected);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            unchanged
        );
    }
}

#[test]
fn local_stream_destinations_are_authored_offline_and_never_print_the_stream_key() {
    const KEY: &str = STREAM_KEY;
    let context = ContractContext::new();
    author_stream_destination(&context);

    let store = ProjectStore::new(&context.project).unwrap();
    let stored = store.load().unwrap();
    let target = &stored.project().stream_targets()[0];
    assert_eq!(target.name(), "Main ingest");
    assert_eq!(target.key().unwrap().expose_secret(), KEY);
    assert_eq!(target.startup(), StartupPolicy::ReconcileDesiredState);
    // Destinations authored without naming a bitrate assume the default.
    assert_eq!(target.video_bitrate_kbps(), 4_500);
    assert!(
        fs::read_to_string(context.project.join("project.json"))
            .unwrap()
            .contains("\"video_bitrate_kbps\": 4500")
    );

    // The inventory line is the operator-facing surface. It must carry enough
    // to identify the destination and none of the secret.
    let streams = invoke_bounded(&["streams", context.project_path()]);
    assert_success(&streams);
    assert_eq!(
        stdout(&streams),
        concat!(
            "stream id=40 name=\"Main ingest\" protocol=rtmps ",
            "url=\"rtmps://ingest.example.test:443/live/****\" ",
            "backup_url=\"rtmps://backup.example.test/live/****\" ",
            "output=30 output_name=\"Primary\" startup=reconcile-desired-state ",
            "video_bitrate_kbps=4500 ladder=none running=false"
        )
    );

    // Only project.json may hold the key; no other command output may.
    let manifest = fs::read_to_string(context.project.join("project.json")).unwrap();
    assert!(manifest.contains(&format!("\"key\": \"{KEY}\"")));
    for output in [
        &streams,
        &invoke_bounded(&["status", context.project_path()]),
        &invoke_bounded(&["outputs", context.project_path()]),
        &invoke_bounded(&["help"]),
        &invoke_bounded(&[
            "stream-add",
            context.project_path(),
            "41",
            "30",
            "http://ingest.example.test/live",
            KEY,
            "Rejected",
        ]),
        &invoke_bounded(&[
            "stream-add",
            context.project_path(),
            "41",
            "30",
            "rtmps://ingest.example.test/live",
            "ab",
            "Rejected",
        ]),
    ] {
        let rendered = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !rendered.contains(KEY),
            "stream key leaked into `{rendered}`"
        );
    }

    assert_stream_authoring_refusals_are_atomic(&context);

    assert_success(&invoke_bounded(&[
        "stream-update",
        context.project_path(),
        "40",
        "30",
        "rtmp://relay.example.test/live",
        "rotated-key-0001",
        "Main ingest",
        "--video-bitrate-kbps",
        "8000",
    ]));
    let updated = store.load().unwrap();
    let target = &updated.project().stream_targets()[0];
    assert_eq!(target.key().unwrap().expose_secret(), "rotated-key-0001");
    assert_eq!(target.redacted_url(), "rtmp://relay.example.test/live/****");
    assert_eq!(target.backup_endpoint(), None);
    assert_eq!(target.startup(), StartupPolicy::Stopped);
    assert_eq!(target.video_bitrate_kbps(), 8_000);

    assert_success(&invoke_bounded(&[
        "stream-remove",
        context.project_path(),
        "40",
    ]));
    assert_eq!(store.load().unwrap().project().stream_targets().len(), 0);
    assert_eq!(
        stdout(&invoke_bounded(&["streams", context.project_path()])),
        ""
    );

    // An srt:// destination authors through the same surface, persists the
    // canonical scheme spelling, and redacts its key into the stream id.
    author_srt_stream_destination(&context);

    assert_success(&invoke_bounded(&[
        "output-remove",
        context.project_path(),
        "30",
    ]));
    fs::remove_dir_all(context.root).unwrap();
}

/// Authors and removes one `srt://` destination on the existing output,
/// asserting the persisted scheme spelling and the redacted inventory line.
fn author_srt_stream_destination(context: &ContractContext) {
    assert_success(&invoke_bounded(&[
        "stream-add",
        context.project_path(),
        "41",
        "30",
        "srt://relay.example.test:9710",
        STREAM_KEY,
        "SRT relay",
    ]));
    let srt_manifest = fs::read_to_string(context.project.join("project.json")).unwrap();
    assert!(srt_manifest.contains("\"protocol\": \"srt\""));
    assert_eq!(
        stdout(&invoke_bounded(&["streams", context.project_path()])),
        concat!(
            "stream id=41 name=\"SRT relay\" protocol=srt ",
            "url=\"srt://relay.example.test:9710?streamid=****\" ",
            "backup_url=\"none\" ",
            "output=30 output_name=\"Primary\" startup=stopped ",
            "video_bitrate_kbps=4500 ladder=none running=false"
        )
    );
    assert_success(&invoke_bounded(&[
        "stream-remove",
        context.project_path(),
        "41",
    ]));
}

/// Authors one `hls:///absolute/dir` destination on the existing output and
/// checks the operator-facing inventory names its local path and authored
/// bitrate without ever rendering a stream key: HLS has none.
#[test]
fn hls_stream_destination_is_authored_offline_and_prints_its_path_and_bitrate() {
    const HLS_DIRECTORY: &str = "/var/lib/freemix/live/hls";
    let context = ContractContext::new();
    author_stream_destination(&context);
    let store = ProjectStore::new(&context.project).unwrap();

    assert_success(&invoke_bounded(&[
        "stream-add",
        context.project_path(),
        "41",
        "30",
        &format!("hls://{HLS_DIRECTORY}"),
        "Local HLS",
        "--video-bitrate-kbps",
        "6000",
    ]));

    let stored = store.load().unwrap();
    let target = stored
        .project()
        .stream_targets()
        .iter()
        .find(|target| target.id().to_string() == "41")
        .expect("the HLS destination was authored");
    assert_eq!(target.protocol(), StreamProtocol::Hls);
    assert_eq!(target.key(), None);
    assert_eq!(target.endpoint().as_str(), HLS_DIRECTORY);
    assert_eq!(target.backup_endpoint(), None);
    assert_eq!(target.video_bitrate_kbps(), 6_000);
    assert_eq!(target.redacted_url(), format!("hls://{HLS_DIRECTORY}"));
    let authored_manifest = fs::read_to_string(context.project.join("project.json")).unwrap();
    assert!(authored_manifest.contains("\"protocol\": \"hls\""));
    assert!(authored_manifest.contains("\"key\": null"));

    // The inventory line is the operator-facing surface: the local directory
    // is storage rather than a secret and is printed verbatim, no `****`
    // placeholder appears, and no key text can.
    let streams = invoke_bounded(&["streams", context.project_path()]);
    assert_success(&streams);
    let rendered = stdout(&streams);
    let hls_line = rendered
        .lines()
        .find(|line| line.contains("id=41"))
        .expect("the HLS destination has an inventory line");
    assert_eq!(
        hls_line,
        concat!(
            "stream id=41 name=\"Local HLS\" protocol=hls ",
            "url=\"hls:///var/lib/freemix/live/hls\" ",
            "backup_url=\"none\" ",
            "output=30 output_name=\"Primary\" startup=stopped ",
            "video_bitrate_kbps=6000 ladder=none running=false"
        )
    );
    assert!(!hls_line.contains("****"));
    assert!(!hls_line.contains(STREAM_KEY));

    fs::remove_dir_all(context.root).unwrap();
}

/// An HLS destination takes no stream key positional and no failover
/// endpoint. Both mistakes are refused by the argument parser, before any
/// project IO, so the bundle stays byte-identical.
#[test]
fn hls_stream_destination_refusals_are_atomic() {
    let context = ContractContext::new();
    author_stream_destination(&context);
    let unchanged = fs::read(context.project.join("project.json")).unwrap();

    for (arguments, expected) in [
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "hls:///var/lib/freemix/live/hls",
                STREAM_KEY,
                "Local HLS",
            ],
            "unexpected argument `Local HLS`",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "hls:///var/lib/freemix/live/hls",
                "Local HLS",
                "--backup",
                "hls:///var/lib/freemix/live/hls-backup",
            ],
            "hls:// stream destinations do not support --backup",
        ),
        (
            vec![
                "stream-update",
                context.project_path(),
                "40",
                "30",
                "hls:///var/lib/freemix/live/hls",
                STREAM_KEY,
                "Main ingest",
            ],
            "unexpected argument `Main ingest`",
        ),
    ] {
        let rejected = invoke_bounded(&arguments);
        assert_failure_contains(&rejected, expected);
        let rendered = format!(
            "{}{}",
            String::from_utf8_lossy(&rejected.stdout),
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert!(
            !rendered.contains(STREAM_KEY),
            "the refused HLS command leaked the stream key into `{rendered}`"
        );
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            unchanged
        );
    }

    fs::remove_dir_all(context.root).unwrap();
}

/// Authors a two-rung ladder on an `hls:///absolute/dir` destination and
/// checks the whole path: the model round-trips both rungs in order, the top
/// rung becomes the destination's advertised bitrate, the manifest persists
/// the ladder array, and the operator-facing inventory prints it without ever
/// rendering a stream key.
#[test]
fn hls_stream_destination_ladder_is_authored_offline_and_printed() {
    const HLS_DIRECTORY: &str = "/var/lib/freemix/live/hls";
    let context = ContractContext::new();
    author_stream_destination(&context);
    let store = ProjectStore::new(&context.project).unwrap();

    assert_success(&invoke_bounded(&[
        "stream-add",
        context.project_path(),
        "41",
        "30",
        &format!("hls://{HLS_DIRECTORY}"),
        "Local HLS",
        "--rung",
        "1000:640x360",
        "--rung",
        "2500:1280x720",
    ]));

    let stored = store.load().unwrap();
    let target = stored
        .project()
        .stream_targets()
        .iter()
        .find(|target| target.id().to_string() == "41")
        .expect("the HLS ladder destination was authored");
    assert_eq!(target.protocol(), StreamProtocol::Hls);
    assert_eq!(target.key(), None);
    assert_eq!(target.backup_endpoint(), None);
    let ladder = target
        .ladder()
        .expect("the HLS destination carries an authored ladder");
    assert_eq!(
        ladder.rungs(),
        [
            StreamLadderRung::new(1_000, 640, 360).unwrap(),
            StreamLadderRung::new(2_500, 1_280, 720).unwrap(),
        ]
    );
    // The top rung is the destination's primary/grouping bitrate.
    assert_eq!(ladder.top_bitrate_kbps(), 2_500);
    assert_eq!(target.video_bitrate_kbps(), 2_500);
    let authored_manifest = manifest(&context.project);
    assert!(
        authored_manifest.contains(concat!(
            "\"ladder\": [",
            "{\"video_bitrate_kbps\": 1000, \"width\": 640, \"height\": 360}, ",
            "{\"video_bitrate_kbps\": 2500, \"width\": 1280, \"height\": 720}",
            "]"
        )),
        "the manifest should persist the ladder array: {authored_manifest}"
    );

    // The inventory line is the operator-facing surface: the ladder and its
    // top bitrate are printed, no `****` placeholder appears, and no key text
    // can.
    let streams = invoke_bounded(&["streams", context.project_path()]);
    assert_success(&streams);
    let rendered = stdout(&streams);
    let hls_line = rendered
        .lines()
        .find(|line| line.contains("id=41"))
        .expect("the ladder destination has an inventory line");
    assert!(
        hls_line.contains("ladder=640x360@1000,1280x720@2500"),
        "the inventory should print the ladder: {hls_line}"
    );
    assert_eq!(
        hls_line,
        concat!(
            "stream id=41 name=\"Local HLS\" protocol=hls ",
            "url=\"hls:///var/lib/freemix/live/hls\" ",
            "backup_url=\"none\" ",
            "output=30 output_name=\"Primary\" startup=stopped ",
            "video_bitrate_kbps=2500 ladder=640x360@1000,1280x720@2500 ",
            "running=false"
        )
    );
    assert!(!hls_line.contains("****"));
    assert!(!hls_line.contains(STREAM_KEY));

    fs::remove_dir_all(context.root).unwrap();
}

/// A ladder is HLS-only and must satisfy the model's rung contract. Every
/// refusal happens during argument parsing, before any project IO, so the
/// bundle stays byte-identical.
#[test]
fn stream_ladder_refusals_are_atomic() {
    let context = ContractContext::new();
    author_stream_destination(&context);
    let unchanged = fs::read(context.project.join("project.json")).unwrap();

    for (arguments, expected) in [
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "rtmp://ingest.example.test/live",
                STREAM_KEY,
                "RTMP with ladder",
                "--rung",
                "1000:640x360",
                "--rung",
                "2500:1280x720",
            ],
            "only hls:// stream destinations support --rung",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "hls:///var/lib/freemix/live/hls",
                "Local HLS",
                "--rung",
                "1000:640x360",
            ],
            "stream ladder must have at least 2 rungs",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "hls:///var/lib/freemix/live/hls",
                "Local HLS",
                "--rung",
                "fast:640x360",
                "--rung",
                "2500:1280x720",
            ],
            "invalid stream ladder rung value `fast:640x360`",
        ),
        (
            vec![
                "stream-add",
                context.project_path(),
                "41",
                "30",
                "hls:///var/lib/freemix/live/hls",
                "Local HLS",
                "--rung",
                "2500:1280x720",
                "--rung",
                "1000:640x360",
            ],
            "stream ladder rung video bitrates must strictly increase",
        ),
    ] {
        let rejected = invoke_bounded(&arguments);
        assert_failure_contains(&rejected, expected);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            unchanged
        );
    }

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_stream_start_stop_round_trip_persists_desired_running() {
    let context = ContractContext::new();
    author_stream_destination(&context);
    let store = ProjectStore::new(&context.project).unwrap();
    assert_eq!(
        json_number(&manifest(&context.project), "schema_version"),
        36
    );

    let started = invoke_bounded(&["stream-start", context.project_path(), "40"]);
    assert_success(&started);
    let started_manifest = manifest(&context.project);
    assert_eq!(json_number(&started_manifest, "schema_version"), 36);
    assert!(started_manifest.contains("\"running\": true"));
    let loaded = store.load().unwrap();
    let target = &loaded.project().stream_targets()[0];
    assert_eq!(target.id().to_string(), "40");
    assert!(target.running());
    let streams_after_start = invoke_bounded(&["streams", context.project_path()]);
    assert_success(&streams_after_start);
    assert!(stdout(&streams_after_start).ends_with("running=true"));

    let stopped = invoke_bounded(&["stream-stop", context.project_path(), "40"]);
    assert_success(&stopped);
    let stopped_manifest = manifest(&context.project);
    assert_eq!(json_number(&stopped_manifest, "schema_version"), 36);
    assert!(stopped_manifest.contains("\"running\": false"));
    assert!(!stopped_manifest.contains("\"running\": true"));
    assert!(!store.load().unwrap().project().stream_targets()[0].running());
    let streams_after_stop = invoke_bounded(&["streams", context.project_path()]);
    assert_success(&streams_after_stop);
    assert!(stdout(&streams_after_stop).ends_with("running=false"));

    // A mistyped or zero id must fail cleanly and leave the bundle untouched.
    for (arguments, expected) in [
        (
            vec!["stream-stop", context.project_path(), "99"],
            "unknown stream destination 99",
        ),
        (
            vec!["stream-start", context.project_path(), "0"],
            "stream destination ID must be nonzero",
        ),
    ] {
        let manifest_before_failure = fs::read(context.project.join("project.json")).unwrap();
        let journal_before = journal_bytes(&store);
        let failure = invoke_bounded(&arguments);
        assert_failure_contains(&failure, expected);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before_failure
        );
        assert_eq!(journal_bytes(&store), journal_before);
    }

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn remote_stream_start_is_accepted_and_replicates_the_streams_roster() {
    let server = FakeRemoteServer::start_stream_start();
    let address = server.address();
    let output = invoke_bounded(&[
        "remote-stream-start",
        &address,
        "40",
        "--key",
        "remote-stream-start",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    let status = stdout(&output);
    assert_remote_status(&status, 1, 1, 1, 2, 2);
    assert!(status.contains(
        r#"Streams=[40:"Main ingest":desired=true:realized=starting:detail=connecting to ingest]"#
    ));
    assert!(
        status.contains(
            "StreamStatus(target=40, realized=starting, connected=false, muxed_bytes=2048, enqueued_pairs=12, dropped_pairs=3, failure=none)"
        ),
        "status output should surface the validated stream-status sample: {status}"
    );

    // A second client reconnecting to the daemon observes the durable roster.
    let observed = invoke_bounded(&["remote-status", &address]);
    assert_success(&observed);
    assert!(
        stdout(&observed)
            .contains(r#"Streams=[40:"Main ingest":desired=true:realized=live:detail=none]"#)
    );
    server.finish();
}

#[test]
fn local_record_start_stop_round_trip_persists_per_slot_recording_desired() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    let store = ProjectStore::new(&context.project).unwrap();
    assert_eq!(
        json_number(&manifest(&context.project), "schema_version"),
        36
    );
    assert!(manifest(&context.project).contains("\"recording_desired\": [false, false]"));

    // The default slot is recorder 1, and only its flag flips.
    let started = invoke_bounded(&["record-start", context.project_path()]);
    assert_success(&started);
    let started_manifest = manifest(&context.project);
    assert_eq!(json_number(&started_manifest, "schema_version"), 36);
    assert!(started_manifest.contains("\"recording_desired\": [true, false]"));
    assert!(stdout(&started).contains("recording_desired=[true,false]"));
    assert_eq!(status(&context.project), stdout(&started));

    // Recorder 2 starts without touching recorder 1, proving the checkpoint
    // mirror writes both slots' flags.
    let second = invoke_bounded(&["record-start", context.project_path(), "--recorder", "2"]);
    assert_success(&second);
    let second_manifest = manifest(&context.project);
    assert_eq!(json_number(&second_manifest, "schema_version"), 36);
    assert!(second_manifest.contains("\"recording_desired\": [true, true]"));
    assert!(stdout(&second).contains("recording_desired=[true,true]"));

    // Stopping recorder 1 leaves recorder 2 recording.
    let stopped = invoke_bounded(&["record-stop", context.project_path()]);
    assert_success(&stopped);
    let stopped_manifest = manifest(&context.project);
    assert_eq!(json_number(&stopped_manifest, "schema_version"), 36);
    assert!(stopped_manifest.contains("\"recording_desired\": [false, true]"));
    assert!(stdout(&stopped).contains("recording_desired=[false,true]"));
    assert_eq!(status(&context.project), stdout(&stopped));

    // A refusal must leave both the manifest and the journal byte-identical.
    store
        .append_batch(&MutationBatch::new(1, 3, 4, b"unapplied".to_vec()))
        .unwrap();
    let manifest_before_failure = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    for arguments in [
        vec!["record-start", context.project_path()],
        vec!["record-stop", context.project_path()],
        vec!["record-start", context.project_path(), "--recorder", "2"],
    ] {
        let failure = invoke_bounded(&arguments);
        assert_failure_contains(
            &failure,
            "project has unapplied journal batches that freemix-cli cannot safely interpret",
        );
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before_failure
        );
        assert_eq!(journal_bytes(&store), journal_before);
    }

    // An out-of-range recorder is refused before the bundle is touched.
    for value in ["0", "3", "two"] {
        let failure =
            invoke_bounded(&["record-start", context.project_path(), "--recorder", value]);
        assert_failure_contains(
            &failure,
            &format!("invalid value for --recorder `{value}`; expected 1 or 2"),
        );
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before_failure
        );
        assert_eq!(journal_bytes(&store), journal_before);
    }

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn repeated_no_op_commands_keep_the_checkpoint_restartable() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    for arguments in [
        vec![
            "scene-input-add",
            context.project_path(),
            "10",
            "10",
            "Program",
        ],
        vec!["audio-bus-add", context.project_path(), "20", "Master"],
        vec![
            "output-add",
            context.project_path(),
            "30",
            "10",
            "20",
            "Primary",
        ],
        vec![
            "stream-add",
            context.project_path(),
            "40",
            "30",
            "rtmps://ingest.example.test:443/live",
            STREAM_KEY,
            "Main ingest",
        ],
    ] {
        assert_success(&invoke_bounded(&arguments));
    }

    // Commands that leave state unchanged are still acceptances. Their
    // durable events must keep revision, event sequence, and runtime
    // generation advancing together, so the next restore still accepts the
    // project. This guards every command family that can be a no-op, not just
    // the record commands that first exposed the counter divergence.
    for arguments in [
        vec!["record-start", context.project_path()],
        vec!["record-start", context.project_path()],
        vec!["record-stop", context.project_path()],
        vec!["record-stop", context.project_path()],
        vec!["preview", context.project_path(), "2"],
        vec!["preview", context.project_path(), "2"],
        vec!["cut", context.project_path()],
        vec!["cut", context.project_path()],
        vec!["ftb", context.project_path(), "live", "30"],
        vec!["ftb", context.project_path(), "live", "30"],
        vec![
            "audio-strip",
            context.project_path(),
            "1",
            "0",
            "0",
            "off",
            "off",
            "on",
            "0",
        ],
        vec![
            "audio-strip",
            context.project_path(),
            "1",
            "0",
            "0",
            "off",
            "off",
            "on",
            "0",
        ],
        vec!["input-reorder", context.project_path(), "1", "2", "10"],
        vec!["input-reorder", context.project_path(), "1", "2", "10"],
        vec!["stream-start", context.project_path(), "40"],
        vec!["stream-start", context.project_path(), "40"],
    ] {
        assert_success(&invoke_bounded(&arguments));
    }
    let status_output = status(&context.project);
    assert!(status_output.contains("recording_desired=[false,false]"));
    assert!(manifest(&context.project).contains("\"recording_desired\": [false, false]"));

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn remote_record_start_is_accepted_and_replicates_the_recording_flag() {
    let server = FakeRemoteServer::start_record_start();
    let address = server.address();
    let output = invoke_bounded(&[
        "remote-record-start",
        &address,
        "--key",
        "remote-record-start",
        "--expect",
        "0",
    ]);
    assert_success(&output);
    let status = stdout(&output);
    assert_remote_status(&status, 1, 1, 1, 2, 2);
    assert!(
        status.contains("Recording=desired=[true,false]"),
        "status output should surface the durable per-slot recording roster: {status}"
    );

    // A second Viewer client reconnecting to the daemon observes the roster.
    let observed = invoke_bounded(&["remote-status", &address]);
    assert_success(&observed);
    let observed_status = stdout(&observed);
    assert!(
        observed_status.contains("Recording=desired=[true,false]"),
        "reconnecting viewer should see the replicated per-slot recording roster: {observed_status}"
    );
    server.finish();
}

#[test]
fn local_output_startup_persists_selected_policy_and_rejects_unknown_output() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    assert_success(&invoke_bounded(&[
        "scene-input-add",
        context.project_path(),
        "10",
        "10",
        "Program",
    ]));
    assert_success(&invoke_bounded(&[
        "audio-bus-add",
        context.project_path(),
        "20",
        "Master",
    ]));
    for (output, name) in [("30", "Primary"), ("31", "Auxiliary")] {
        assert_success(&invoke_bounded(&[
            "output-add",
            context.project_path(),
            output,
            "10",
            "20",
            name,
        ]));
    }

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    assert_success(&invoke_bounded(&[
        "output-startup",
        context.project_path(),
        "30",
        "reconcile-desired-state",
    ]));
    let after = store.load().unwrap();
    assert_eq!(
        after.project().outputs()[0].startup,
        StartupPolicy::ReconcileDesiredState
    );
    assert_eq!(after.project().outputs()[1], before.project().outputs()[1]);
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let outputs = invoke_bounded(&["outputs", context.project_path()]);
    assert_success(&outputs);
    assert!(stdout(&outputs).contains("output id=30 name=\"Primary\" video_scene=10 video_scene_name=\"Program\" audio_bus=20 audio_bus_name=\"Master\" startup=reconcile-desired-state capabilities=[]"));

    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let unknown = invoke_bounded(&["output-startup", context.project_path(), "999", "stopped"]);
    assert_failure_contains(&unknown, "unknown output 999");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_output_editing_persists_routes_and_names_and_rejects_unknown_references() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "10",
        "10",
        "Wide \"A\"",
    ]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "11",
        "11",
        "Close",
    ]));
    assert_success(&invoke(&[
        "audio-bus-add",
        context.project_path(),
        "20",
        "Master",
    ]));
    assert_success(&invoke(&[
        "audio-bus-add",
        context.project_path(),
        "21",
        "Aux \"A\"",
    ]));
    assert_success(&invoke(&[
        "output-add",
        context.project_path(),
        "30",
        "10",
        "20",
        "Program \"Main\"",
    ]));
    assert_success(&invoke(&[
        "output-add",
        context.project_path(),
        "31",
        "11",
        "21",
        "Auxiliary",
    ]));
    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    assert_eq!(before.project().audio_buses().len(), 2);
    assert_eq!(
        before.project().audio_buses()[0].id,
        BusId::new(NonZeroU128::new(20).unwrap())
    );
    assert_eq!(before.project().audio_buses()[0].name, "Master");
    assert_eq!(before.project().audio_buses()[0].sends.len(), 0);
    assert_eq!(
        before.project().audio_buses()[1].id,
        BusId::new(NonZeroU128::new(21).unwrap())
    );
    assert_eq!(before.project().audio_buses()[1].name, "Aux \"A\"");
    assert_eq!(before.project().audio_buses()[1].sends.len(), 0);
    assert_eq!(before.project().outputs().len(), 2);
    assert_eq!(
        before.project().outputs()[0].id,
        OutputId::new(NonZeroU128::new(30).unwrap())
    );
    assert_eq!(
        before.project().outputs()[0].video_source,
        SceneId::new(NonZeroU128::new(10).unwrap())
    );
    assert_eq!(
        before.project().outputs()[0].audio_source,
        BusId::new(NonZeroU128::new(20).unwrap())
    );
    assert_eq!(
        before.project().outputs()[0].startup,
        StartupPolicy::Stopped
    );
    assert_eq!(before.project().outputs()[0].required_capabilities.len(), 0);
    assert_eq!(
        before.project().outputs()[1].id,
        OutputId::new(NonZeroU128::new(31).unwrap())
    );

    let changed = invoke(&["output-route", context.project_path(), "30", "11", "21"]);
    assert_success(&changed);
    let after = store.load().unwrap();
    assert_eq!(
        after.project().outputs()[0].video_source,
        SceneId::new(NonZeroU128::new(11).unwrap())
    );
    assert_eq!(
        after.project().outputs()[0].audio_source,
        BusId::new(NonZeroU128::new(21).unwrap())
    );
    assert_eq!(after.project().outputs()[1], before.project().outputs()[1]);
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let supplied = "  Program Final  ";
    let renamed = invoke_bounded(&["output-rename", context.project_path(), "30", supplied]);
    assert_success(&renamed);
    let renamed = store.load().unwrap();
    let mut expected_project = after.project().clone();
    expected_project
        .rename_output(
            OutputId::new(NonZeroU128::new(30).unwrap()),
            supplied.into(),
        )
        .unwrap();
    assert_eq!(renamed.project(), &expected_project);
    assert_eq!(renamed.runtime_routing(), after.runtime_routing());
    assert_eq!(
        renamed.runtime_manual_transitions(),
        after.runtime_manual_transitions()
    );
    assert_eq!(
        renamed.runtime_fade_to_black(),
        after.runtime_fade_to_black()
    );
    assert_eq!(renamed.runtime_overlays(), after.runtime_overlays());
    assert_eq!(renamed.position(), after.position());
    assert_eq!(renamed.idempotency_receipts(), after.idempotency_receipts());
    let idempotent = invoke_bounded(&["output-rename", context.project_path(), "30", supplied]);
    assert_success(&idempotent);
    let idempotent = store.load().unwrap();
    assert_eq!(idempotent.project(), renamed.project());
    assert_eq!(idempotent.runtime_routing(), renamed.runtime_routing());
    assert_eq!(
        idempotent.runtime_manual_transitions(),
        renamed.runtime_manual_transitions()
    );
    assert_eq!(
        idempotent.runtime_fade_to_black(),
        renamed.runtime_fade_to_black()
    );
    assert_eq!(idempotent.runtime_overlays(), renamed.runtime_overlays());
    assert_eq!(idempotent.position(), renamed.position());
    assert_eq!(
        idempotent.idempotency_receipts(),
        renamed.idempotency_receipts()
    );
    assert_success(&invoke(&["status", context.project_path()]));
    let audio_buses = invoke(&["audio-buses", context.project_path()]);
    assert_success(&audio_buses);
    assert_eq!(
        stdout(&audio_buses),
        concat!(
            "audio_bus id=20 name=\"Master\" sends=[]\n",
            "audio_bus id=21 name=\"Aux \\\"A\\\"\" sends=[]"
        )
    );
    let outputs = invoke(&["outputs", context.project_path()]);
    assert_success(&outputs);
    assert_eq!(
        stdout(&outputs),
        concat!(
            "output id=30 name=\"  Program Final  \" video_scene=11 video_scene_name=\"Close\" audio_bus=21 audio_bus_name=\"Aux \\\"A\\\"\" startup=stopped capabilities=[]\n",
            "output id=31 name=\"Auxiliary\" video_scene=11 video_scene_name=\"Close\" audio_bus=21 audio_bus_name=\"Aux \\\"A\\\"\" startup=stopped capabilities=[]"
        )
    );
    let scenes = invoke(&["scenes", context.project_path()]);
    assert_success(&scenes);
    assert_eq!(
        stdout(&scenes),
        concat!(
            "scene id=10 name=\"Wide \\\"A\\\"\" background=rgba(0,0,0,255) layers=0\n",
            "scene id=11 name=\"Close\" background=rgba(0,0,0,255) layers=0",
        )
    );

    let unchanged_manifest = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke_bounded(&["output-rename", context.project_path(), "30", "aUxIlIaRy"]);
    assert_failure_contains(&rejected, "duplicate output name");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        unchanged_manifest
    );
    let rejected = invoke(&["audio-bus-add", context.project_path(), "22", "mAsTeR"]);
    assert_failure_contains(&rejected, "duplicate audio bus name");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        unchanged_manifest
    );
    let rejected = invoke(&[
        "output-add",
        context.project_path(),
        "32",
        "10",
        "20",
        "  pRoGrAm fInAl  ",
    ]);
    assert_failure_contains(&rejected, "duplicate output name");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        unchanged_manifest
    );
    let rejected = invoke(&[
        "output-add",
        context.project_path(),
        "32",
        "999",
        "20",
        "New",
    ]);
    assert_failure_contains(&rejected, "unknown scene 999");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        unchanged_manifest
    );
    let rejected = invoke(&[
        "output-add",
        context.project_path(),
        "32",
        "10",
        "999",
        "New",
    ]);
    assert_failure_contains(&rejected, "unknown audio bus 999");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        unchanged_manifest
    );
    assert_success(&invoke(&["status", context.project_path()]));

    let assert_runtime_preserved = |actual: &StoredProject, expected: &StoredProject| {
        assert_eq!(actual.runtime_routing(), expected.runtime_routing());
        assert_eq!(
            actual.runtime_manual_transitions(),
            expected.runtime_manual_transitions()
        );
        assert_eq!(
            actual.runtime_fade_to_black(),
            expected.runtime_fade_to_black()
        );
        assert_eq!(actual.runtime_overlays(), expected.runtime_overlays());
        assert_eq!(actual.position(), expected.position());
        assert_eq!(
            actual.idempotency_receipts(),
            expected.idempotency_receipts()
        );
    };
    let before_remove = store.load().unwrap();
    assert_success(&invoke(&["output-remove", context.project_path(), "31"]));
    let after_output_remove = store.load().unwrap();
    assert_eq!(
        after_output_remove
            .project()
            .outputs()
            .iter()
            .map(|output| output.id)
            .collect::<Vec<_>>(),
        vec![OutputId::new(NonZeroU128::new(30).unwrap())]
    );
    assert_runtime_preserved(&after_output_remove, &before_remove);
    assert_success(&invoke(&["status", context.project_path()]));
    assert_success(&invoke(&["output-remove", context.project_path(), "30"]));
    assert_success(&invoke(&["audio-bus-remove", context.project_path(), "21"]));
    let after_bus_remove = store.load().unwrap();
    assert_eq!(after_bus_remove.project().outputs().len(), 0);
    assert_eq!(
        after_bus_remove
            .project()
            .audio_buses()
            .iter()
            .map(|bus| bus.id)
            .collect::<Vec<_>>(),
        vec![BusId::new(NonZeroU128::new(20).unwrap())]
    );
    assert_runtime_preserved(&after_bus_remove, &after_output_remove);
    assert_success(&invoke(&["status", context.project_path()]));

    assert_success(&invoke(&[
        "audio-bus-add",
        context.project_path(),
        "30",
        "Output-bound",
    ]));
    assert_success(&invoke(&[
        "output-add",
        context.project_path(),
        "40",
        "10",
        "30",
        "Bound",
    ]));
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    let rejected = invoke(&["audio-bus-remove", context.project_path(), "30"]);
    assert_failure_contains(&rejected, "used by output 40");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);

    assert_success(&invoke(&[
        "overlay-output",
        context.project_path(),
        "1",
        "40",
        "true",
    ]));
    let overlays = store.load().unwrap().runtime_overlays().clone();
    assert!(overlays.desired.iter().any(|channel| {
        channel
            .included_outputs
            .contains(&OutputId::new(NonZeroU128::new(40).unwrap()))
    }));
    assert!(overlays.realized.iter().any(|channel| {
        channel
            .included_outputs
            .contains(&OutputId::new(NonZeroU128::new(40).unwrap()))
    }));
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    let rejected = invoke(&["output-remove", context.project_path(), "40"]);
    assert_failure_contains(&rejected, "runtime overlay reference");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);

    let stored = store.load().unwrap();
    let mut project = stored.project().clone();
    project.add_audio_bus(AudioBus {
        id: BusId::new(NonZeroU128::new(50).unwrap()),
        name: "Send source".into(),
        sends: vec![BusSend {
            destination: BusId::new(NonZeroU128::new(51).unwrap()),
        }],
    });
    project.add_audio_bus(AudioBus {
        id: BusId::new(NonZeroU128::new(51).unwrap()),
        name: "Send destination".into(),
        sends: Vec::new(),
    });
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )
    .unwrap();
    store.save(&configured).unwrap();
    for (bus, expected) in [
        ("50", "sends to audio bus 51"),
        ("51", "sends to audio bus 51"),
    ] {
        let manifest_before = fs::read(context.project.join("project.json")).unwrap();
        let journal_before = journal_bytes(&store);
        let rejected = invoke(&["audio-bus-remove", context.project_path(), bus]);
        assert_failure_contains(&rejected, expected);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before
        );
        assert_eq!(journal_bytes(&store), journal_before);
    }
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_audio_bus_send_editing_is_atomic() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    for (id, name) in [("20", "Master"), ("21", "Aux")] {
        assert_success(&invoke(&[
            "audio-bus-add",
            context.project_path(),
            id,
            name,
        ]));
    }
    let edit = |command, source, destination| {
        assert_success(&invoke(&[
            command,
            context.project_path(),
            source,
            destination,
        ]));
    };
    let assert_inventory = |expected| {
        let inventory = invoke(&["audio-buses", context.project_path()]);
        assert_success(&inventory);
        assert_eq!(stdout(&inventory), expected);
    };
    edit("audio-bus-send-add", "20", "21");
    assert_inventory(
        "audio_bus id=20 name=\"Master\" sends=[21]\naudio_bus id=21 name=\"Aux\" sends=[]",
    );
    edit("audio-bus-send-remove", "20", "21");
    assert_inventory(
        "audio_bus id=20 name=\"Master\" sends=[]\naudio_bus id=21 name=\"Aux\" sends=[]",
    );
    edit("audio-bus-send-add", "20", "21");
    let before_cycle = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke(&["audio-bus-send-add", context.project_path(), "21", "20"]);
    assert_failure_contains(&rejected, "audio bus send would create a cycle");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        before_cycle
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn project_rename_preserves_runtime_state_and_rejects_blank_name() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&[
        "new",
        context.project_path(),
        "--name",
        "Original Show",
    ]));
    assert_success(&invoke_bounded(&[
        "tbar-start",
        context.project_path(),
        "fade",
        "--key",
        "project-rename-runtime",
        "--expect",
        "0",
    ]));

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    let journal_before = journal_bytes(&store);
    assert!(before.runtime_manual_transitions().desired.is_some());
    assert!(before.position().revision > 0);
    assert_ne!(before.idempotency_receipts().len(), 0);

    let supplied = "  Final \"Show\"  ";
    assert_success(&invoke_bounded(&[
        "project-rename",
        context.project_path(),
        supplied,
    ]));
    let after = store.load().unwrap();
    assert_eq!(after.project().name(), supplied);
    assert_eq!(
        after.project().schema_version(),
        before.project().schema_version()
    );
    assert_eq!(after.project().id(), before.project().id());
    assert_eq!(after.project().settings(), before.project().settings());
    assert_eq!(after.project().inputs(), before.project().inputs());
    assert_eq!(
        after.project().input_audio_strips(),
        before.project().input_audio_strips()
    );
    assert_eq!(after.project().scenes(), before.project().scenes());
    assert_eq!(
        after.project().audio_buses(),
        before.project().audio_buses()
    );
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    assert_eq!(after.project().stingers(), before.project().stingers());
    assert_eq!(
        after.project().restart_policy(),
        before.project().restart_policy()
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    assert_eq!(journal_bytes(&store), journal_before);

    assert_success(&invoke_bounded(&[
        "project-rename",
        context.project_path(),
        supplied,
    ]));
    assert_eq!(store.load().unwrap(), after);
    assert_eq!(journal_bytes(&store), journal_before);

    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke_bounded(&["project-rename", context.project_path(), "   "]);
    assert_failure_contains(&rejected, "project name must not be blank");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_audio_bus_rename_persists_exact_name_and_preserves_runtime_state() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    for (id, name) in [("20", "Master"), ("21", "Return")] {
        assert_success(&invoke_bounded(&[
            "audio-bus-add",
            context.project_path(),
            id,
            name,
        ]));
    }
    assert_success(&invoke_bounded(&[
        "audio-bus-send-add",
        context.project_path(),
        "20",
        "21",
    ]));
    assert_success(&invoke_bounded(&[
        "scene-input-add",
        context.project_path(),
        "10",
        "10",
        "Program",
    ]));
    for (output, bus, name) in [("30", "20", "Primary"), ("31", "21", "Return")] {
        assert_success(&invoke_bounded(&[
            "output-add",
            context.project_path(),
            output,
            "10",
            bus,
            name,
        ]));
    }

    let store = ProjectStore::new(&context.project).unwrap();
    assert_success(&invoke_bounded(&[
        "tbar-start",
        context.project_path(),
        "slide",
        "--key",
        "audio-bus-rename-runtime",
        "--expect",
        "0",
    ]));
    let before = store.load().unwrap();
    let supplied = "  Aux \"Return\"  ";
    assert_success(&invoke_bounded(&[
        "audio-bus-rename",
        context.project_path(),
        "21",
        supplied,
    ]));
    let after = store.load().unwrap();
    let before_buses = before.project().audio_buses();
    let after_buses = after.project().audio_buses();
    assert_eq!(
        after_buses.iter().map(|bus| bus.id).collect::<Vec<_>>(),
        before_buses.iter().map(|bus| bus.id).collect::<Vec<_>>()
    );
    assert_eq!(after_buses[0], before_buses[0]);
    assert_eq!(after_buses[1].id, before_buses[1].id);
    assert_eq!(after_buses[1].sends, before_buses[1].sends);
    assert_eq!(after_buses[1].name, supplied);
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());

    let buses = invoke_bounded(&["audio-buses", context.project_path()]);
    assert_success(&buses);
    assert!(stdout(&buses).contains("audio_bus id=21 name=\"  Aux \\\"Return\\\"  \""));
    let outputs = invoke_bounded(&["outputs", context.project_path()]);
    assert_success(&outputs);
    assert!(stdout(&outputs).contains("audio_bus=21 audio_bus_name=\"  Aux \\\"Return\\\"  \""));

    assert_success(&invoke_bounded(&[
        "audio-bus-rename",
        context.project_path(),
        "21",
        supplied,
    ]));
    assert_eq!(store.load().unwrap(), after);

    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke_bounded(&["audio-bus-rename", context.project_path(), "21", "mAsTeR"]);
    assert_failure_contains(&rejected, "duplicate audio bus name");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_inputs_reports_ordered_inventory() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "input-add",
        context.project_path(),
        "3",
        "Aux \"B\"",
    ]));

    let inventory = invoke(&["inputs", context.project_path()]);
    assert_success(&inventory);
    assert_eq!(
        stdout(&inventory),
        concat!(
            "input id=1 name=\"Input 1\" kind=simulated strip=gain_mdb=0,balance_bp=0,delay_samples=0,muted=false,soloed=false,follow_video=true\n",
            "input id=2 name=\"Input 2\" kind=simulated strip=gain_mdb=0,balance_bp=0,delay_samples=0,muted=false,soloed=false,follow_video=true\n",
            "input id=3 name=\"Aux \\\"B\\\"\" kind=simulated strip=gain_mdb=0,balance_bp=0,delay_samples=0,muted=false,soloed=false,follow_video=true",
        )
    );

    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_input_add_persists_empty_scene_without_routing() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "audio-strip",
        context.project_path(),
        "2",
        "-1200",
        "2500",
        "on",
        "off",
        "off",
        "480",
    ]));
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let name = "  Scene  input  ";
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        name,
    ]));
    let after = ProjectStore::new(&context.project).unwrap().load().unwrap();

    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    let scene = after.project().scenes().last().unwrap();
    assert_eq!(scene.id.get().get(), 7);
    assert_eq!(scene.name, name);
    assert_eq!(scene.background, Rgba8::OPAQUE_BLACK);
    assert_eq!(scene.layers.len(), 0);
    let input = after.project().inputs().last().unwrap();
    assert_eq!(input.id.get().get(), 3);
    assert_eq!(input.name, name);
    assert!(
        matches!(input.kind, InputKind::Scene { scene_id, audio_source: None } if scene_id.get().get() == 7)
    );
    assert_eq!(
        after.project().input_audio_strip(input.id).unwrap(),
        InputAudioStripState::default()
    );
    assert_eq!(
        after
            .project()
            .scenes()
            .iter()
            .map(|scene| scene.id.get().get())
            .collect::<Vec<_>>(),
        [7]
    );
    assert_eq!(
        after
            .project()
            .inputs()
            .iter()
            .map(|input| input.id.get().get())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );

    let before_duplicate = manifest(&context.project);
    let store = ProjectStore::new(&context.project).unwrap();
    let stored_before_duplicate = store.load().unwrap();
    let journal_before_duplicate = journal_bytes(&store);
    let duplicate = invoke(&["scene-input-add", context.project_path(), "3", "8", "Other"]);
    assert_failure_contains(&duplicate, "input 3 already exists");
    assert_eq!(manifest(&context.project), before_duplicate);
    let stored_after_duplicate = store.load().unwrap();
    assert_eq!(
        stored_after_duplicate.project(),
        stored_before_duplicate.project()
    );
    assert_eq!(
        stored_after_duplicate.runtime_routing(),
        stored_before_duplicate.runtime_routing()
    );
    assert_eq!(
        stored_after_duplicate.runtime_manual_transitions(),
        stored_before_duplicate.runtime_manual_transitions()
    );
    assert_eq!(
        stored_after_duplicate.runtime_fade_to_black(),
        stored_before_duplicate.runtime_fade_to_black()
    );
    assert_eq!(
        stored_after_duplicate.runtime_overlays(),
        stored_before_duplicate.runtime_overlays()
    );
    assert_eq!(
        stored_after_duplicate.position(),
        stored_before_duplicate.position()
    );
    assert_eq!(
        stored_after_duplicate.idempotency_receipts(),
        stored_before_duplicate.idempotency_receipts()
    );
    assert_eq!(journal_bytes(&store), journal_before_duplicate);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_input_duplicate_persists_scene_pair_and_default_strip() {
    let context = ContractContext::new();
    let path = context.project_path();
    let run = |args: &[&str]| assert_success(&invoke(args));
    run(&["new", path]);
    run(&["scene-input-add", path, "10", "10", "Source"]);
    run(&["scene-layer-add", path, "10", "1", "2", "Inset"]);
    run(&["audio-bus-add", path, "20", "Master"]);
    run(&["output-add", path, "30", "10", "20", "Program"]);
    run(&["tbar-start", path, "fade"]);
    run(&["tbar-position", path, "5000"]);
    run(&["ftb", path, "black", "2"]);

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    let source_scene = before.project().scenes()[0].clone();
    run(&[
        "scene-input-duplicate",
        path,
        "10",
        "11",
        "11",
        "Copied Scene",
        "Copied Input",
    ]);
    let after = store.load().unwrap();
    let persisted_after = (
        fs::read(context.project.join("project.json")).unwrap(),
        journal_bytes(&store),
    );
    let copied_scene = &after.project().scenes()[1];
    let mut expected_scene = source_scene.clone();
    expected_scene.id = SceneId::new(NonZeroU128::new(11).unwrap());
    expected_scene.name = "Copied Scene".into();
    assert_eq!(copied_scene, &expected_scene);
    let copied_input = &after.project().inputs()[3];
    assert_eq!(copied_input.id, InputId::new(NonZeroU128::new(11).unwrap()));
    assert_eq!(copied_input.name, "Copied Input");
    assert_eq!(copied_input.required_capabilities.len(), 0);
    assert!(matches!(
        copied_input.kind,
        InputKind::Scene { scene_id, audio_source: None } if scene_id == expected_scene.id
    ));
    assert_eq!(
        after.project().input_audio_strip(copied_input.id),
        Some(InputAudioStripState::default())
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    for (command, needle) in [
        ("scenes", "scene id=11 name=\"Copied Scene\""),
        ("inputs", "input id=11 name=\"Copied Input\" kind=scene"),
    ] {
        let inventory = invoke(&[command, context.project_path()]);
        assert_success(&inventory);
        assert!(stdout(&inventory).contains(needle));
    }
    assert!(status(&context.project).contains("AudioStrips=[1:"));

    let rejected = invoke(&[
        "scene-input-duplicate",
        context.project_path(),
        "99",
        "12",
        "12",
        "Rejected Scene",
        "Rejected Input",
    ]);
    assert_failure_contains(&rejected, "unknown source scene 99");
    assert_eq!(
        (
            fs::read(context.project.join("project.json")).unwrap(),
            journal_bytes(&store),
        ),
        persisted_after
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn scene_rename_persists_exact_name_and_failures_preserve_manifest() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    for (input, scene, name) in [("10", "10", "Wide"), ("11", "11", "Close")] {
        assert_success(&invoke(&[
            "scene-input-add",
            context.project_path(),
            input,
            scene,
            name,
        ]));
    }
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "10",
        "1",
        "2",
        "Camera layer",
    ]));
    assert_success(&invoke(&[
        "scene-layer-geometry",
        context.project_path(),
        "10",
        "0",
        "10",
        "20",
        "640",
        "360",
        "90",
    ]));
    assert_success(&invoke(&[
        "scene-layer-appearance",
        context.project_path(),
        "10",
        "0",
        "off",
        "200",
    ]));
    assert_success(&invoke(&[
        "scene-background",
        context.project_path(),
        "10",
        "16",
        "8",
        "4",
        "16",
    ]));
    assert_success(&invoke(&[
        "audio-bus-add",
        context.project_path(),
        "20",
        "Master",
    ]));
    assert_success(&invoke(&[
        "output-add",
        context.project_path(),
        "30",
        "10",
        "20",
        "Program",
    ]));
    assert_success(&invoke(&["preview", context.project_path(), "2"]));
    assert_success(&invoke(&["cut", context.project_path()]));
    assert_success(&invoke(&["tbar-start", context.project_path(), "fade"]));
    assert_success(&invoke(&["tbar-position", context.project_path(), "5000"]));
    assert_success(&invoke(&["ftb", context.project_path(), "black", "2"]));
    assert_success(&invoke(&[
        "overlay-transition",
        context.project_path(),
        "1",
        "fade",
        "3",
    ]));
    assert_success(&invoke(&[
        "overlay-appearance",
        context.project_path(),
        "1",
        "top-left",
        "thick-white",
    ]));
    assert_success(&invoke(&["overlay-take", context.project_path(), "1", "1"]));
    assert_success(&invoke(&[
        "overlay-output",
        context.project_path(),
        "1",
        "30",
        "true",
    ]));
    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    assert_ne!(before.project().scenes()[0].background, Rgba8::OPAQUE_BLACK);
    assert_eq!(before.project().scenes()[0].layers.len(), 1);
    assert_ne!(before.project().audio_buses().len(), 0);
    assert_ne!(before.project().outputs().len(), 0);
    assert!(before.runtime_routing().desired_program_id.is_some());
    assert!(before.runtime_manual_transitions().desired.is_some());
    assert!(before.runtime_fade_to_black().desired.target_active);
    assert!(before.runtime_overlays().desired[0].source.is_some());
    assert_ne!(
        before.runtime_overlays().desired[0].included_outputs.len(),
        0
    );
    assert!(before.position().revision > 0);
    assert_ne!(before.idempotency_receipts().len(), 0);
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    for (scene, name, expected) in [
        ("99", "Missing", "scene 99 does not exist"),
        ("10", "close", "scene name is already in use"),
    ] {
        let rejected = invoke(&["scene-rename", context.project_path(), scene, name]);
        assert_failure_contains(&rejected, expected);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before
        );
        assert_eq!(journal_bytes(&store), journal_before);
    }
    let rejected = invoke(&["scene-rename", context.project_path(), "10", "   "]);
    assert_failure_contains(&rejected, "scene name must not be blank");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);

    let supplied = "  Exact scene  ";
    assert_success(&invoke(&[
        "scene-rename",
        context.project_path(),
        "10",
        supplied,
    ]));
    let after = store.load().unwrap();
    let mut expected_project = before.project().clone();
    expected_project
        .rename_scene(SceneId::new(NonZeroU128::new(10).unwrap()), supplied.into())
        .unwrap();
    assert_eq!(after.project(), &expected_project);
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let scenes = invoke(&["scenes", context.project_path()]);
    assert_success(&scenes);
    assert!(stdout(&scenes).contains("name=\"  Exact scene  \""));
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_input_remove_is_atomic_across_input_and_scene() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    assert_success(&invoke_bounded(&[
        "input-add",
        context.project_path(),
        "3",
        "Unrelated",
    ]));
    assert_success(&invoke_bounded(&[
        "audio-strip",
        context.project_path(),
        "3",
        "-1200",
        "2500",
        "on",
        "off",
        "off",
        "480",
    ]));
    for (input, scene, name) in [("9", "9", "Keep"), ("10", "10", "Remove")] {
        assert_success(&invoke_bounded(&[
            "scene-input-add",
            context.project_path(),
            input,
            scene,
            name,
        ]));
    }

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    assert_success(&invoke_bounded(&[
        "scene-input-remove",
        context.project_path(),
        "10",
    ]));
    let after = store.load().unwrap();
    let removed_input = InputId::new(NonZeroU128::new(10).unwrap());
    let removed_scene = SceneId::new(NonZeroU128::new(10).unwrap());
    assert_eq!(
        after.project().inputs(),
        before
            .project()
            .inputs()
            .iter()
            .filter(|input| input.id != removed_input)
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().input_audio_strips(),
        before
            .project()
            .input_audio_strips()
            .iter()
            .filter(|strip| strip.input != removed_input)
            .copied()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().scenes(),
        before
            .project()
            .scenes()
            .iter()
            .filter(|scene| scene.id != removed_scene)
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().schema_version(),
        before.project().schema_version()
    );
    assert_eq!(after.project().id(), before.project().id());
    assert_eq!(after.project().name(), before.project().name());
    assert_eq!(after.project().settings(), before.project().settings());
    assert_eq!(
        after.project().audio_buses(),
        before.project().audio_buses()
    );
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    assert_eq!(after.project().stingers(), before.project().stingers());
    assert_eq!(
        after.project().restart_policy(),
        before.project().restart_policy()
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());

    assert_success(&invoke_bounded(&[
        "scene-input-add",
        context.project_path(),
        "11",
        "11",
        "Blocked",
    ]));
    assert_success(&invoke_bounded(&[
        "scene-layer-add-scene",
        context.project_path(),
        "9",
        "11",
        "0",
        "External reference",
    ]));
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    let rejected = invoke_bounded(&["scene-input-remove", context.project_path(), "11"]);
    assert_failure_contains(&rejected, "scene 9 layer references scene 11");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    let non_scene = invoke_bounded(&["scene-input-remove", context.project_path(), "3"]);
    assert_failure_contains(&non_scene, "input 3 is not a scene input");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_removal_preserves_runtime_and_rejects_references() {
    fn prepare(reference: Option<&str>) -> ContractContext {
        let context = ContractContext::new();
        assert_success(&invoke(&["new", context.project_path()]));
        for (input, scene, name) in [("10", "10", "Keep"), ("11", "11", "Other")] {
            assert_success(&invoke(&[
                "scene-input-add",
                context.project_path(),
                input,
                scene,
                name,
            ]));
        }
        assert_success(&invoke(&[
            "audio-bus-add",
            context.project_path(),
            "20",
            "Master",
        ]));
        assert_success(&invoke(&[
            "output-add",
            context.project_path(),
            "30",
            "10",
            "20",
            "Program",
        ]));
        assert_success(&invoke(&[
            "tbar-start",
            context.project_path(),
            "wipe",
            "--key",
            "scene-remove-tbar",
            "--expect",
            "0",
        ]));
        assert_success(&invoke(&[
            "tbar-position",
            context.project_path(),
            "5000",
            "--key",
            "scene-remove-position",
            "--expect",
            "1",
        ]));
        assert_success(&invoke(&[
            "ftb",
            context.project_path(),
            "black",
            "2",
            "--key",
            "scene-remove-ftb",
            "--expect",
            "2",
        ]));
        assert_success(&invoke(&[
            "overlay-output",
            context.project_path(),
            "1",
            "30",
            "true",
        ]));

        let store = ProjectStore::new(&context.project).unwrap();
        let stored = store.load().unwrap();
        let mut project = stored.project().clone();
        project.add_scene(Scene {
            id: SceneId::new(NonZeroU128::new(12).unwrap()),
            name: "Target".into(),
            background: Rgba8::OPAQUE_BLACK,
            layers: Vec::new(),
            animation: fm_model::SceneAnimation::default(),
        });
        if reference == Some("input") {
            project.add_input(Input {
                id: InputId::new(NonZeroU128::new(12).unwrap()),
                name: "Target input".into(),
                kind: InputKind::Scene {
                    scene_id: SceneId::new(NonZeroU128::new(12).unwrap()),
                    audio_source: None,
                },
                required_capabilities: Vec::new(),
            });
        }
        let configured = StoredProject::from_project_with_complete_runtime_state(
            project,
            stored.runtime_routing(),
            stored.runtime_manual_transitions(),
            stored.runtime_fade_to_black(),
            stored.runtime_overlays().clone(),
            stored.position(),
            stored.idempotency_receipts().to_vec(),
        )
        .unwrap();
        store.save(&configured).unwrap();
        match reference {
            Some("layer") => {
                assert_success(&invoke(&[
                    "scene-layer-add",
                    context.project_path(),
                    "11",
                    "10",
                    "0",
                    "Target layer",
                ]));
                assert_success(&invoke(&[
                    "scene-layer-source-scene",
                    context.project_path(),
                    "11",
                    "0",
                    "12",
                ]));
            }
            Some("output") => assert_success(&invoke(&[
                "output-route",
                context.project_path(),
                "30",
                "12",
                "20",
            ])),
            _ => {}
        }
        context
    }

    let success_context = prepare(None);
    let success_store = ProjectStore::new(&success_context.project).unwrap();
    let before = success_store.load().unwrap();
    let removed = invoke(&["scene-remove", success_context.project_path(), "12"]);
    assert_success(&removed);
    assert_success(&invoke(&["status", success_context.project_path()]));
    let after = success_store.load().unwrap();
    assert_eq!(
        after
            .project()
            .scenes()
            .iter()
            .map(|scene| scene.id)
            .collect::<Vec<_>>(),
        [
            SceneId::new(NonZeroU128::new(10).unwrap()),
            SceneId::new(NonZeroU128::new(11).unwrap())
        ]
    );
    let mut expected_project = before.project().clone();
    expected_project
        .remove_scene(SceneId::new(NonZeroU128::new(12).unwrap()))
        .unwrap();
    assert_eq!(after.project(), &expected_project);
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let scenes = invoke(&["scenes", success_context.project_path()]);
    assert_success(&scenes);
    assert!(!stdout(&scenes).contains("id=12"));
    fs::remove_dir_all(success_context.root).unwrap();

    for (reference, expected) in [
        ("input", "input 12 references scene 12"),
        ("layer", "scene 11 layer references scene 12"),
        ("output", "output 30 references scene 12"),
        ("unknown", "unknown scene 99"),
    ] {
        let context = prepare((reference != "unknown").then_some(reference));
        let store = ProjectStore::new(&context.project).unwrap();
        let manifest_before = fs::read(context.project.join("project.json")).unwrap();
        let journal_before = journal_bytes(&store);
        let rejected = invoke(&[
            "scene-remove",
            context.project_path(),
            if reference == "unknown" { "99" } else { "12" },
        ]);
        assert_failure_contains(&rejected, expected);
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before
        );
        assert_eq!(journal_bytes(&store), journal_before);
        fs::remove_dir_all(context.root).unwrap();
    }
}

#[test]
fn local_scene_input_audio_source() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    for (input, scene, name) in [("3", "7", "A"), ("4", "8", "B")] {
        assert_success(&invoke(&[
            "scene-input-add",
            context.project_path(),
            input,
            scene,
            name,
        ]));
    }
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_success(&invoke(&[
        "scene-input-audio-source",
        context.project_path(),
        "3",
        "1",
    ]));
    let assigned = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert!(matches!(
        assigned.project().inputs().iter().find(|input| input.id.get().get() == 3).unwrap().kind,
        InputKind::Scene { audio_source: Some(source), .. } if source.get().get() == 1
    ));
    let mut normalized = assigned.project().clone();
    normalized
        .set_scene_input_audio_source(InputId::new(NonZeroU128::new(3).unwrap()), None)
        .unwrap();
    assert_eq!(&normalized, before.project());
    assert_eq!(assigned.runtime_routing(), before.runtime_routing());
    assert_eq!(
        assigned.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        assigned.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(assigned.runtime_overlays(), before.runtime_overlays());
    assert_eq!(assigned.position(), before.position());
    assert_eq!(
        assigned.idempotency_receipts(),
        before.idempotency_receipts()
    );

    assert_success(&invoke(&[
        "scene-input-audio-source-clear",
        context.project_path(),
        "3",
    ]));
    let cleared = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_eq!(cleared.project(), before.project());

    assert_success(&invoke(&[
        "scene-input-audio-source",
        context.project_path(),
        "3",
        "4",
    ]));
    let cycle_manifest = manifest(&context.project);
    let cycle = invoke(&["scene-input-audio-source", context.project_path(), "4", "3"]);
    assert_failure_contains(&cycle, "audio source would create a cycle");
    assert_eq!(manifest(&context.project), cycle_manifest);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_background() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "2",
        "Layer",
    ]));
    assert_success(&invoke(&[
        "audio-bus-add",
        context.project_path(),
        "20",
        "Master",
    ]));
    assert_success(&invoke(&[
        "audio-bus-add",
        context.project_path(),
        "21",
        "Aux",
    ]));
    assert_success(&invoke(&[
        "output-add",
        context.project_path(),
        "30",
        "7",
        "20",
        "Program",
    ]));
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_success(&invoke(&[
        "scene-background",
        context.project_path(),
        "7",
        "16",
        "8",
        "4",
        "16",
    ]));
    let after = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    assert_eq!(after.project().inputs(), before.project().inputs());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    assert_eq!(
        after.project().audio_buses(),
        before.project().audio_buses()
    );
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(
        after
            .project()
            .scenes()
            .iter()
            .map(|scene| scene.id)
            .collect::<Vec<_>>(),
        before
            .project()
            .scenes()
            .iter()
            .map(|scene| scene.id)
            .collect::<Vec<_>>()
    );
    let mut expected_scenes = before.project().scenes().to_vec();
    expected_scenes
        .iter_mut()
        .find(|scene| scene.id.get().get() == 7)
        .unwrap()
        .background = Rgba8::new(16, 8, 4, 16);
    assert_eq!(after.project().scenes(), expected_scenes);
    let unchanged = manifest(&context.project);
    let journal_unchanged = journal_bytes(&ProjectStore::new(&context.project).unwrap());
    let invalid = invoke(&[
        "scene-background",
        context.project_path(),
        "7",
        "17",
        "8",
        "4",
        "16",
    ]);
    assert_failure_contains(&invalid, "scene background must be premultiplied");
    assert_eq!(manifest(&context.project), unchanged);
    assert_eq!(
        journal_bytes(&ProjectStore::new(&context.project).unwrap()),
        journal_unchanged
    );
    let unknown_manifest = manifest(&context.project);
    let unknown_journal = journal_bytes(&ProjectStore::new(&context.project).unwrap());
    let unknown = invoke(&[
        "scene-background",
        context.project_path(),
        "999",
        "17",
        "8",
        "4",
        "16",
    ]);
    assert_failure_contains(&unknown, "unknown scene 999");
    assert_eq!(manifest(&context.project), unknown_manifest);
    assert_eq!(
        journal_bytes(&ProjectStore::new(&context.project).unwrap()),
        unknown_journal
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_add_persists_full_frame_layer() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let name = "  Full frame layer  ";
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "-4",
        name,
    ]));
    let after = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let scene = after
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id.get().get() == 7)
        .unwrap();
    assert_eq!(scene.layers.len(), 1);
    let layer = &scene.layers[0];
    assert_eq!(layer.name, name);
    assert_eq!(
        layer.source,
        SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap()))
    );
    assert!(layer.enabled);
    assert_eq!(
        layer.geometry,
        LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0)
    );
    assert_eq!(layer.crop, None);
    assert_eq!(layer.mask, None);
    assert_eq!(layer.opacity, u8::MAX);
    assert_eq!(layer.z_order, -4);
    let before_unknown = manifest(&context.project);
    let unknown = invoke(&[
        "scene-layer-add",
        context.project_path(),
        "99",
        "1",
        "2",
        "other",
    ]);
    assert_failure_contains(&unknown, "unknown scene");
    assert_eq!(manifest(&context.project), before_unknown);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_add_scene_persists_exact_source() {
    let context = ContractContext::new();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));
    let path = context.project_path();
    run(&["new", path]);
    run(&["scene-input-add", path, "10", "10", "Source"]);
    run(&["scene-input-add", path, "11", "11", "Target"]);
    run(&["scene-layer-add", path, "11", "1", "-4", "  Input layer  "]);
    run(&[
        "scene-layer-add-scene",
        path,
        "11",
        "10",
        "-4",
        "  Source scene  ",
    ]);

    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let scene_id = |value| SceneId::new(NonZeroU128::new(value).unwrap());
    let target = stored
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id == scene_id(11))
        .unwrap();
    assert_eq!(target.id, scene_id(11));
    assert_eq!(target.layers.len(), 2);
    let input_layer = &target.layers[0];
    assert_eq!(
        input_layer.source,
        SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap()))
    );
    assert_eq!(input_layer.name, "  Input layer  ");
    assert!(input_layer.enabled);
    assert_eq!(
        input_layer.geometry,
        LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0)
    );
    assert_eq!(input_layer.crop, None);
    assert_eq!(input_layer.mask, None);
    assert_eq!(input_layer.opacity, u8::MAX);
    assert_eq!(input_layer.z_order, -4);
    let scene_layer = &target.layers[1];
    assert_eq!(scene_layer.source, SourceRef::Scene(scene_id(10)));
    assert_eq!(scene_layer.name, "  Source scene  ");
    assert!(scene_layer.enabled);
    assert_eq!(
        scene_layer.geometry,
        LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0)
    );
    assert_eq!(scene_layer.crop, None);
    assert_eq!(scene_layer.mask, None);
    assert_eq!(scene_layer.opacity, u8::MAX);
    assert_eq!(scene_layer.z_order, -4);
    let before_self_reference = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke_bounded(&["scene-layer-add-scene", path, "11", "11", "0", "Self"]);
    assert_failure_contains(&rejected, "cycle");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        before_self_reference
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_add_rejects_missing_input_without_manifest_or_journal_change() {
    let context = ContractContext::new();
    let path = context.project_path();
    assert_success(&invoke(&["new", path]));
    assert_success(&invoke(&["scene-input-add", path, "10", "7", "Scene"]));
    let store = ProjectStore::new(&context.project).unwrap();
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);

    let rejected = invoke(&["scene-layer-add", path, "7", "99", "0", "Missing"]);
    assert_failure_contains(&rejected, "error: missing source input 99");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);

    let blank_name = invoke(&["scene-layer-add", path, "7", "7", "0", " \t"]);
    assert_failure_contains(&blank_name, "error: layer name must not be blank");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_layer_rename_persists_exact_name_and_rejects_invalid_target() {
    let context = ContractContext::new();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));
    run(&["new", context.project_path()]);
    run(&["scene-input-add", context.project_path(), "3", "7", "Scene"]);
    let store = ProjectStore::new(&context.project).unwrap();
    let stored = store.load().unwrap();
    let mut project = stored.project().clone();
    project
        .add_layer_to_scene(
            SceneId::new(NonZeroU128::new(7).unwrap()),
            Layer {
                id: fm_model::LayerId::new(1).expect("layer id one is nonzero"),
                name: "Original".into(),
                source: SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap())),
                enabled: false,
                geometry: LayerGeometry::new(-12, 34, 640, 480, Rotation::Deg270),
                crop: Some(fm_model::CropRect::new(10, 20, 640, 480)),
                mask: Some(RectMask::new(10, 20, 600, 400).inverted(true)),
                effects: Vec::new(),
                opacity: 96,
                z_order: 9,
            },
        )
        .unwrap();
    store
        .save(
            &StoredProject::from_project_with_complete_runtime_state(
                project,
                stored.runtime_routing(),
                stored.runtime_manual_transitions(),
                stored.runtime_fade_to_black(),
                stored.runtime_overlays().clone(),
                stored.position(),
                stored.idempotency_receipts().to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    run(&[
        "tbar-start",
        context.project_path(),
        "slide",
        "--key",
        "scene-layer-rename-runtime",
        "--expect",
        "0",
    ]);

    let before = store.load().unwrap();
    assert!(before.runtime_manual_transitions().desired.is_some());
    assert!(before.position().revision > 0);
    assert_ne!(before.idempotency_receipts().len(), 0);
    let supplied = "  Exact layer  ";
    run(&[
        "scene-layer-rename",
        context.project_path(),
        "7",
        "0",
        supplied,
    ]);
    let after = store.load().unwrap();
    let mut expected_layer = before.project().scenes()[0].layers[0].clone();
    expected_layer.name = supplied.into();
    assert_eq!(after.project().scenes()[0].layers[0], expected_layer);
    let after_scenes = after.project().scenes();
    let before_scenes = before.project().scenes();
    assert!(
        after_scenes
            .iter()
            .map(|s| (&s.id, &s.name, &s.background, s.layers.len()))
            .eq(before_scenes
                .iter()
                .map(|s| (&s.id, &s.name, &s.background, s.layers.len())))
    );
    assert_eq!(
        (
            after.runtime_routing(),
            after.runtime_manual_transitions(),
            after.runtime_fade_to_black(),
            after.runtime_overlays(),
            after.position(),
            after.idempotency_receipts(),
        ),
        (
            before.runtime_routing(),
            before.runtime_manual_transitions(),
            before.runtime_fade_to_black(),
            before.runtime_overlays(),
            before.position(),
            before.idempotency_receipts(),
        )
    );

    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke_bounded(&[
        "scene-layer-rename",
        context.project_path(),
        "7",
        "1",
        "Rejected",
    ]);
    assert_failure_contains(
        &rejected,
        "layer index 1 out of range for scene 7 with 1 layers",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_duplicate_copies_configured_layer() {
    let context = ContractContext::new();
    let path = context.project_path();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));
    run(&["new", path]);
    run(&["scene-input-add", path, "3", "7", "Scene"]);
    run(&["scene-layer-add", path, "7", "1", "-4", "Original"]);
    run(&["scene-layer-appearance", path, "7", "0", "off", "96"]);
    run(&[
        "scene-layer-geometry",
        path,
        "7",
        "0",
        "-12",
        "34",
        "640",
        "480",
        "270",
    ]);
    run(&["scene-layer-crop", path, "7", "0", "10", "20", "600", "400"]);
    run(&[
        "scene-layer-mask",
        path,
        "7",
        "0",
        "20",
        "30",
        "500",
        "300",
        "inverted",
    ]);
    run(&[
        "tbar-start",
        path,
        "slide",
        "--key",
        "scene-layer-duplicate-runtime",
        "--expect",
        "0",
    ]);

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    run(&["scene-layer-duplicate", path, "7", "0", "  Duplicate  "]);
    let after = store.load().unwrap();
    assert_eq!(
        (
            after.runtime_routing(),
            after.runtime_manual_transitions(),
            after.runtime_fade_to_black(),
            after.runtime_overlays(),
            after.position(),
            after.idempotency_receipts(),
        ),
        (
            before.runtime_routing(),
            before.runtime_manual_transitions(),
            before.runtime_fade_to_black(),
            before.runtime_overlays(),
            before.position(),
            before.idempotency_receipts(),
        )
    );
    let before_layer = &before.project().scenes()[0].layers[0];
    let after_layers = &after.project().scenes()[0].layers;
    let mut expected_duplicate = before_layer.clone();
    expected_duplicate.name = "  Duplicate  ".into();
    // The duplicate gets the scene's next stable layer id.
    expected_duplicate.id = fm_model::LayerId::new(2).unwrap();
    assert_eq!(after_layers, &[before_layer.clone(), expected_duplicate]);

    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let rejected = invoke_bounded(&["scene-layer-duplicate", path, "7", "2", "Rejected"]);
    assert_failure_contains(
        &rejected,
        "layer index 2 out of range for scene 7 with 2 layers",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_copy_preserves_configured_layer_and_rejects_cycle() {
    let context = ContractContext::new();
    let path = context.project_path();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));
    run(&["new", path]);
    run(&["scene-input-add", path, "10", "10", "Source"]);
    run(&["scene-input-add", path, "11", "11", "Target"]);
    run(&["scene-layer-add", path, "10", "1", "-4", "Original"]);
    run(&["scene-layer-appearance", path, "10", "0", "off", "96"]);
    run(&[
        "scene-layer-geometry",
        path,
        "10",
        "0",
        "-12",
        "34",
        "640",
        "480",
        "270",
    ]);
    run(&[
        "scene-layer-crop",
        path,
        "10",
        "0",
        "10",
        "20",
        "600",
        "400",
    ]);
    run(&[
        "scene-layer-mask",
        path,
        "10",
        "0",
        "20",
        "30",
        "500",
        "300",
        "inverted",
    ]);
    for name in ["First", "Second"] {
        run(&["scene-layer-add", path, "11", "1", "4", name]);
    }
    run(&[
        "tbar-start",
        path,
        "slide",
        "--key",
        "scene-layer-copy-runtime",
        "--expect",
        "0",
    ]);

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    run(&["scene-layer-copy", path, "10", "0", "11", "  Copied  "]);
    let after = store.load().unwrap();
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    let source_before = &before.project().scenes()[0].layers;
    let target_before = &before.project().scenes()[1].layers;
    assert_eq!(&after.project().scenes()[0].layers, source_before);
    let mut copied = source_before[0].clone();
    copied.name = "  Copied  ".into();
    // The copy gets the target scene's next stable layer id.
    copied.id = fm_model::LayerId::new(3).unwrap();
    assert_eq!(
        &after.project().scenes()[1].layers,
        &[target_before[0].clone(), target_before[1].clone(), copied]
    );

    run(&[
        "scene-layer-add-scene",
        path,
        "10",
        "11",
        "0",
        "Target source",
    ]);
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    let rejected = invoke_bounded(&["scene-layer-copy", path, "10", "1", "11", "Rejected"]);
    assert_failure_contains(&rejected, "scene layer source would create a cycle");
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_move_changes_only_stable_tie_order() {
    let context = ContractContext::new();
    let path = context.project_path();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));
    run(&["new", path]);
    run(&["scene-input-add", path, "3", "7", "Scene"]);
    for name in ["First", "Second"] {
        run(&["scene-layer-add", path, "7", "1", "4", name]);
    }
    run(&["scene-layer-duplicate", path, "7", "1", "Third"]);

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    let before_runtime = (
        before.runtime_routing(),
        before.runtime_manual_transitions(),
        before.runtime_fade_to_black(),
        before.runtime_overlays(),
        before.position(),
        before.idempotency_receipts(),
    );
    let mut expected_layers = before.project().scenes()[0].layers.clone();
    let moved = expected_layers.remove(2);
    expected_layers.insert(0, moved);

    run(&["scene-layer-move", path, "7", "2", "0"]);
    let after = store.load().unwrap();
    assert_eq!(after.project().scenes()[0].layers, expected_layers);
    assert_eq!(
        (
            after.runtime_routing(),
            after.runtime_manual_transitions(),
            after.runtime_fade_to_black(),
            after.runtime_overlays(),
            after.position(),
            after.idempotency_receipts(),
        ),
        before_runtime
    );

    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    let rejected = invoke_bounded(&["scene-layer-move", path, "7", "0", "3"]);
    assert_failure_contains(
        &rejected,
        "layer index 3 out of range for scene 7 with 3 layers",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_layer_source_reassignment() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    for (input, scene, name) in [("3", "7", "A"), ("4", "8", "B"), ("5", "9", "Target")] {
        assert_success(&invoke(&[
            "scene-input-add",
            context.project_path(),
            input,
            scene,
            name,
        ]));
    }
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "9",
        "1",
        "4",
        "first",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "9",
        "2",
        "-2",
        "second",
    ]));
    assert_success(&invoke(&[
        "scene-layer-source-scene",
        context.project_path(),
        "9",
        "1",
        "8",
    ]));
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_success(&invoke(&[
        "scene-layer-source-scene",
        context.project_path(),
        "9",
        "0",
        "7",
    ]));
    assert_success(&invoke(&[
        "scene-layer-source-input",
        context.project_path(),
        "9",
        "1",
        "2",
    ]));
    let after = ProjectStore::new(&context.project).unwrap().load().unwrap();
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let scene_id = |value| fm_types::SceneId::new(NonZeroU128::new(value).unwrap());
    let mut normalized = after.project().clone();
    normalized
        .set_scene_layer_source(
            scene_id(9),
            0,
            SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap())),
        )
        .unwrap();
    normalized
        .set_scene_layer_source(scene_id(9), 1, SourceRef::Scene(scene_id(8)))
        .unwrap();
    assert_eq!(&normalized, before.project());
    let target = after
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id == scene_id(9))
        .unwrap();
    assert_eq!(target.layers[0].source, SourceRef::Scene(scene_id(7)));
    assert_eq!(
        target.layers[1].source,
        SourceRef::Input(InputId::new(NonZeroU128::new(2).unwrap()))
    );
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "0",
        "nested",
    ]));
    let before_cycle = fs::read(context.project.join("project.json")).unwrap();
    assert_failure_contains(
        &invoke(&[
            "scene-layer-source-input",
            context.project_path(),
            "7",
            "0",
            "3",
        ]),
        "cycle",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        before_cycle
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_remove_preserves_remaining_order() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "4",
        "First",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "9",
        "Second",
    ]));
    let before = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_success(&invoke(&[
        "scene-layer-remove",
        context.project_path(),
        "7",
        "0",
    ]));
    let after = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let scene = after
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id.get().get() == 7)
        .unwrap();
    assert_eq!(scene.layers.len(), 1);
    let layer = &scene.layers[0];
    assert_eq!(layer.name, "Second");
    assert_eq!(
        layer.source,
        SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap()))
    );
    assert!(layer.enabled);
    assert_eq!(
        layer.geometry,
        LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0)
    );
    assert_eq!(layer.crop, None);
    assert_eq!(layer.mask, None);
    assert_eq!(layer.opacity, u8::MAX);
    assert_eq!(layer.z_order, 9);
    let before_out_of_range = manifest(&context.project);
    let out_of_range = invoke(&["scene-layer-remove", context.project_path(), "7", "1"]);
    assert_failure_contains(&out_of_range, "out of range");
    assert_eq!(manifest(&context.project), before_out_of_range);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_appearance_preserves_layer_identity() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    for name in ["First", "Second"] {
        assert_success(&invoke(&[
            "scene-layer-add",
            context.project_path(),
            "7",
            "1",
            "4",
            name,
        ]));
    }
    let before = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    let before_scene = before.project().scenes().last().unwrap().clone();
    assert_success(&invoke(&[
        "scene-layer-appearance",
        context.project_path(),
        "7",
        "1",
        "off",
        "96",
    ]));
    let after = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    let after_scene = after
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id.get().get() == 7)
        .unwrap();
    let mut expected_layers = before_scene.layers;
    expected_layers[1].enabled = false;
    expected_layers[1].opacity = 96;
    assert_eq!(after_scene.layers, expected_layers);
    let before_invalid = manifest(&context.project);
    assert_failure_contains(
        &invoke(&[
            "scene-layer-appearance",
            context.project_path(),
            "7",
            "1",
            "on",
            "256",
        ]),
        "opacity must be in 0..=255",
    );
    assert_eq!(manifest(&context.project), before_invalid);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_geometry_rejects_invalid_values_without_persistence() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    for (z_order, name) in [(4, "First"), (9, "Second")] {
        assert_success(&invoke(&[
            "scene-layer-add",
            context.project_path(),
            "7",
            "1",
            &z_order.to_string(),
            name,
        ]));
    }
    let before = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    let manifest_before_failure = fs::read(context.project.join("project.json")).unwrap();
    let journal_before_failure = journal_bytes(&ProjectStore::new(&context.project).unwrap());
    for (width, height) in [("0", "480"), ("640", "8193")] {
        let invalid = invoke(&[
            "scene-layer-geometry",
            context.project_path(),
            "7",
            "1",
            "-12",
            "34",
            width,
            height,
            "270",
        ]);
        assert_failure_contains(&invalid, "invalid geometry for layer 1 in scene 7");
        assert_eq!(
            fs::read(context.project.join("project.json")).unwrap(),
            manifest_before_failure
        );
        assert_eq!(
            journal_bytes(&ProjectStore::new(&context.project).unwrap()),
            journal_before_failure
        );
    }
    assert_success(&invoke(&[
        "scene-layer-geometry",
        context.project_path(),
        "7",
        "1",
        "-12",
        "34",
        "640",
        "480",
        "270",
    ]));
    let after = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        (
            after.runtime_routing(),
            after.runtime_manual_transitions(),
            after.runtime_fade_to_black(),
            after.runtime_overlays(),
            after.position(),
            after.idempotency_receipts(),
        ),
        (
            before.runtime_routing(),
            before.runtime_manual_transitions(),
            before.runtime_fade_to_black(),
            before.runtime_overlays(),
            before.position(),
            before.idempotency_receipts(),
        )
    );
    let before_scene = before.project().scenes().last().unwrap();
    let after_scene = after.project().scenes().last().unwrap();
    let mut expected_layers = before_scene.layers.clone();
    expected_layers[1].geometry = LayerGeometry::new(-12, 34, 640, 480, Rotation::Deg270);
    assert_eq!(after_scene.layers, expected_layers);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_z_order_preserves_vector_and_runtime() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    for (z_order, name) in [(4, "First"), (9, "Second")] {
        assert_success(&invoke(&[
            "scene-layer-add",
            context.project_path(),
            "7",
            "1",
            &z_order.to_string(),
            name,
        ]));
    }
    let before = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    let before_scene = before.project().scenes().last().unwrap();
    let before_runtime = (
        before.runtime_routing(),
        before.runtime_manual_transitions(),
        before.runtime_fade_to_black(),
        before.runtime_overlays(),
        before.position(),
        before.idempotency_receipts(),
    );
    assert_success(&invoke(&[
        "scene-layer-z-order",
        context.project_path(),
        "7",
        "1",
        &i32::MIN.to_string(),
    ]));
    let after = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        (
            after.runtime_routing(),
            after.runtime_manual_transitions(),
            after.runtime_fade_to_black(),
            after.runtime_overlays(),
            after.position(),
            after.idempotency_receipts(),
        ),
        before_runtime
    );
    let after_scene = after.project().scenes().last().unwrap();
    let mut expected_layers = before_scene.layers.clone();
    expected_layers[1].z_order = i32::MIN;
    assert_eq!(after_scene.layers, expected_layers);

    assert_success(&invoke(&[
        "scene-layer-z-order",
        context.project_path(),
        "7",
        "1",
        "4",
    ]));
    let repeated = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        (
            repeated.runtime_routing(),
            repeated.runtime_manual_transitions(),
            repeated.runtime_fade_to_black(),
            repeated.runtime_overlays(),
            repeated.position(),
            repeated.idempotency_receipts(),
        ),
        before_runtime
    );
    let repeated_scene = repeated.project().scenes().last().unwrap();
    expected_layers[1].z_order = 4;
    assert_eq!(repeated_scene.layers, expected_layers);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_scene_layer_crop_checked_preserves_manifest_and_journal() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "4",
        "Layer",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "2",
        "5",
        "Other layer",
    ]));
    let before = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_success(&invoke(&[
        "scene-layer-crop",
        context.project_path(),
        "7",
        "0",
        "10",
        "20",
        "640",
        "480",
    ]));
    let cropped = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        (
            cropped.runtime_routing(),
            cropped.runtime_manual_transitions(),
            cropped.runtime_fade_to_black(),
            cropped.runtime_overlays(),
            cropped.position(),
            cropped.idempotency_receipts(),
        ),
        (
            before.runtime_routing(),
            before.runtime_manual_transitions(),
            before.runtime_fade_to_black(),
            before.runtime_overlays(),
            before.position(),
            before.idempotency_receipts(),
        )
    );
    let mut expected_layers = before.project().scenes()[0].layers.clone();
    expected_layers[0].crop = Some(fm_model::CropRect::new(10, 20, 640, 480));
    assert_eq!(cropped.project().scenes()[0].layers, expected_layers);

    assert_success(&invoke(&[
        "scene-layer-crop-clear",
        context.project_path(),
        "7",
        "0",
    ]));
    let cleared = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        cleared.project().scenes()[0].layers,
        before.project().scenes()[0].layers
    );

    let before_invalid = manifest(&context.project);
    let store = ProjectStore::new(context.project_path()).unwrap();
    let journal_before_invalid = journal_bytes(&store);
    assert_failure_contains(
        &invoke(&[
            "scene-layer-crop",
            context.project_path(),
            "7",
            "0",
            "10",
            "20",
            "2000",
            "480",
        ]),
        "invalid crop for layer 0 in scene 7",
    );
    assert_eq!(manifest(&context.project), before_invalid);
    assert_eq!(journal_bytes(&store), journal_before_invalid);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_layer_mask_checked_preserves_manifest_and_journal() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "scene-input-add",
        context.project_path(),
        "3",
        "7",
        "Scene",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "1",
        "4",
        "Layer",
    ]));
    assert_success(&invoke(&[
        "scene-layer-add",
        context.project_path(),
        "7",
        "2",
        "5",
        "Other layer",
    ]));
    assert_success(&invoke(&[
        "scene-layer-crop",
        context.project_path(),
        "7",
        "0",
        "10",
        "20",
        "640",
        "480",
    ]));
    let before = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_success(&invoke(&[
        "scene-layer-mask",
        context.project_path(),
        "7",
        "0",
        "10",
        "20",
        "600",
        "400",
        "inverted",
    ]));
    let masked = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        (
            masked.runtime_routing(),
            masked.runtime_manual_transitions(),
            masked.runtime_fade_to_black(),
            masked.runtime_overlays(),
            masked.position(),
            masked.idempotency_receipts(),
        ),
        (
            before.runtime_routing(),
            before.runtime_manual_transitions(),
            before.runtime_fade_to_black(),
            before.runtime_overlays(),
            before.position(),
            before.idempotency_receipts(),
        )
    );
    let mut expected_layers = before.project().scenes()[0].layers.clone();
    expected_layers[0].mask = Some(RectMask::new(10, 20, 600, 400).inverted(true));
    assert_eq!(masked.project().scenes()[0].layers, expected_layers);

    assert_success(&invoke(&[
        "scene-layer-mask-clear",
        context.project_path(),
        "7",
        "0",
    ]));
    let cleared = ProjectStore::new(context.project_path())
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        cleared.project().scenes()[0].layers,
        before.project().scenes()[0].layers
    );

    let before_invalid = manifest(&context.project);
    let store = ProjectStore::new(context.project_path()).unwrap();
    let journal_before_invalid = journal_bytes(&store);
    assert_failure_contains(
        &invoke(&[
            "scene-layer-mask",
            context.project_path(),
            "7",
            "0",
            "640",
            "0",
            "1",
            "1",
            "normal",
        ]),
        "invalid mask for layer 0 in scene 7",
    );
    assert_eq!(manifest(&context.project), before_invalid);
    assert_eq!(journal_bytes(&store), journal_before_invalid);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_scene_layer_effects_author_render_and_clear_exactly() {
    let root = unique_test_root();
    let project = root.join("effects.freemix");
    let plain = root.join("plain.ppm");
    let leveled = root.join("leveled.ppm");
    let replaced = root.join("replaced.ppm");
    let cleared = root.join("cleared.ppm");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    macro_rules! apply {
        ($arguments:expr) => {
            assert_success(&invoke_bounded($arguments));
        };
    }

    apply!(&["new", path]);
    // Scene renders compose at the project size before scaling to the output.
    // The effect math is resolution-independent, so a 4x4 project keeps each
    // 4x4 render a 1:1 composition instead of a full 1920x1080 debug-build
    // composite per render.
    let mut source = manifest(&project);
    replace_once(&mut source, "\"width\": 1920,", "\"width\": 4,");
    replace_once(&mut source, "\"height\": 1080,", "\"height\": 4,");
    fs::write(project.join("project.json"), source).unwrap();
    // A dark-gray source proves the exact effect math: `levels 0 128` maps 64
    // to exactly 128, so the rendered pixel is mid-gray rather than the source.
    apply!(&["input-replace-solid", path, "1", "64", "64", "64", "255"]);
    apply!(&["scene-input-add", path, "3", "11", "Program"]);
    apply!(&["scene-layer-add", path, "11", "1", "0", "Layer"]);
    apply!(&["preview", path, "3"]);
    apply!(&["cut", path]);
    apply!(&[
        "render",
        path,
        plain.to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    assert_solid_ppm(&plain, 4, 4, [64, 64, 64]);

    apply!(&[
        "scene-layer-effect-add",
        path,
        "11",
        "0",
        "levels",
        "0",
        "128",
    ]);
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(
        stored.project().scenes()[0].layers[0].effects,
        vec![LayerEffect::new("levels", vec![0, 128])]
    );
    let scenes = invoke(&["scenes", path]);
    assert_success(&scenes);
    assert!(stdout(&scenes).contains("effects=levels(0,128)"));
    apply!(&[
        "render",
        path,
        leveled.to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    assert_solid_ppm(&leveled, 4, 4, [128, 128, 128]);

    // A second effect appends after the first and a replacement keeps the
    // order, so the render sees `levels 0 64` then `saturation 50`.
    apply!(&[
        "scene-layer-effect-add",
        path,
        "11",
        "0",
        "saturation",
        "50"
    ]);
    apply!(&[
        "scene-layer-effect-set",
        path,
        "11",
        "0",
        "0",
        "levels",
        "0",
        "64",
    ]);
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(
        stored.project().scenes()[0].layers[0].effects,
        vec![
            LayerEffect::new("levels", vec![0, 64]),
            LayerEffect::new("saturation", vec![50]),
        ]
    );
    let scenes = invoke(&["scenes", path]);
    assert_success(&scenes);
    assert!(stdout(&scenes).contains("effects=levels(0,64);saturation(50)"));
    apply!(&[
        "render",
        path,
        replaced.to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    assert_solid_ppm(&replaced, 4, 4, [255, 255, 255]);

    // Removing the second effect leaves the first, and clearing removes both,
    // restoring the unmodified render exactly.
    apply!(&["scene-layer-effect-remove", path, "11", "0", "1"]);
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(
        stored.project().scenes()[0].layers[0].effects,
        vec![LayerEffect::new("levels", vec![0, 64])]
    );
    apply!(&["scene-layer-effect-clear", path, "11", "0"]);
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(stored.project().scenes()[0].layers[0].effects.len(), 0);
    let scenes = invoke(&["scenes", path]);
    assert_success(&scenes);
    assert!(stdout(&scenes).contains("effects=none"));
    apply!(&[
        "render",
        path,
        cleared.to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    assert_solid_ppm(&cleared, 4, 4, [64, 64, 64]);

    // Unknown-but-bounded names are preserved for a later executor, while a
    // bounded rejection leaves the manifest byte-identical.
    apply!(&[
        "scene-layer-effect-add",
        path,
        "11",
        "0",
        "future.effect",
        "1",
        "-2",
    ]);
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(
        stored.project().scenes()[0].layers[0].effects,
        vec![LayerEffect::new("future.effect", vec![1, -2])]
    );
    // The persisted unknown name reaches the executor, which rejects it
    // without publishing a frame.
    let unsupported = invoke(&[
        "render",
        path,
        root.join("unsupported.ppm").to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);
    assert_failure_contains(&unsupported, "future.effect");
    assert!(!root.join("unsupported.ppm").exists());
    let before = fs::read(project.join("project.json")).unwrap();
    let too_many_parameters = (0..=LayerEffect::MAX_PARAMETERS)
        .map(|parameter| parameter.to_string())
        .collect::<Vec<_>>();
    let mut arguments = vec!["scene-layer-effect-add", path, "11", "0", "levels"];
    arguments.extend(too_many_parameters.iter().map(String::as_str));
    assert_failure_contains(
        &invoke_bounded(&arguments),
        "effect must not carry more than 32 parameters",
    );
    assert_failure_contains(
        &invoke(&["scene-layer-effect-add", path, "11", "0", ""]),
        "effect name must not be empty",
    );
    assert_failure_contains(
        &invoke(&["scene-layer-effect-remove", path, "11", "0", "9"]),
        "effect index 9 out of range for layer 0 in scene 11 with 1 effects",
    );
    assert_failure_contains(
        &invoke(&[
            "scene-layer-effect-set",
            path,
            "11",
            "0",
            "9",
            "levels",
            "0",
            "64",
        ]),
        "effect index 9 out of range for layer 0 in scene 11 with 1 effects",
    );
    assert_eq!(fs::read(project.join("project.json")).unwrap(), before);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_input_duplicate_copies_source_and_strip() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "audio-strip",
        context.project_path(),
        "2",
        "-1200",
        "2500",
        "on",
        "off",
        "off",
        "480",
    ]));

    let duplicate = invoke(&[
        "input-duplicate",
        context.project_path(),
        "2",
        "3",
        "Duplicated source",
    ]);
    assert_success(&duplicate);

    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let inputs = stored.project().inputs();
    assert_eq!(
        inputs
            .iter()
            .map(|input| input.id.get().get())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let source = &inputs[1];
    let copy = &inputs[2];
    assert_eq!(copy.name, "Duplicated source");
    assert_eq!(copy.kind, source.kind);
    assert_eq!(copy.required_capabilities, source.required_capabilities);
    assert_eq!(
        stored.project().input_audio_strip(copy.id),
        stored.project().input_audio_strip(source.id)
    );
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_input_replace_simulated_preserves_identity_and_runtime() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let input = InputId::new(NonZeroU128::new(2).unwrap());
    let before_input = before
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert!(matches!(
        &before_input.kind,
        InputKind::Simulated(SimulatedInput {
            video: SimulatedVideo::Solid(_),
            audio: SimulatedAudio::Sine { .. }
        })
    ));

    assert_success(&invoke_bounded(&[
        "input-replace-simulated",
        context.project_path(),
        "2",
    ]));
    let store = ProjectStore::new(&context.project).unwrap();
    let after = store.load().unwrap();
    let after_input = after
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert_eq!(after_input.id, before_input.id);
    assert_eq!(after_input.name, before_input.name);
    assert_eq!(
        after.project().input_audio_strip(input),
        before.project().input_audio_strip(input)
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());
    assert!(matches!(
        &after_input.kind,
        InputKind::Simulated(SimulatedInput {
            video: SimulatedVideo::Bars,
            audio: SimulatedAudio::Silence
        })
    ));

    let unchanged = manifest(&context.project);
    let journal_before = journal_bytes(&store);
    assert_failure_contains(
        &invoke_bounded(&["input-replace-simulated", context.project_path(), "999"]),
        "unknown input 999",
    );
    assert_eq!(manifest(&context.project), unchanged);
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_input_replace_solid_preserves_identity_and_runtime() {
    let context = ContractContext::new();
    assert_success(&invoke_bounded(&[
        "new",
        context.project_path(),
        "--name",
        "Solid Replace Show",
    ]));
    assert_success(&invoke_bounded(&[
        "audio-strip",
        context.project_path(),
        "2",
        "-1200",
        "2500",
        "on",
        "off",
        "off",
        "480",
    ]));
    assert_success(&invoke_bounded(&[
        "tbar-start",
        context.project_path(),
        "fade",
        "--key",
        "replace-solid-runtime",
        "--expect",
        "1",
    ]));
    let before = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let input = InputId::new(NonZeroU128::new(2).unwrap());
    let before_input = before
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert!(matches!(
        &before_input.kind,
        InputKind::Simulated(SimulatedInput {
            video: SimulatedVideo::Solid(_),
            audio: SimulatedAudio::Sine { .. }
        })
    ));

    assert_success(&invoke_bounded(&[
        "input-replace-solid",
        context.project_path(),
        "2",
        "1",
        "127",
        "254",
        "0",
    ]));
    let after = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let after_input = after
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert_eq!(after_input.id, before_input.id);
    assert_eq!(after_input.name, before_input.name);
    assert_eq!(after_input.required_capabilities, Vec::<String>::new());
    assert_eq!(
        after_input.kind,
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(1, 127, 254, 0)),
            SimulatedAudio::Silence,
        ))
    );
    assert_eq!(
        after.project().schema_version(),
        before.project().schema_version()
    );
    assert_eq!(after.project().id(), before.project().id());
    assert_eq!(after.project().name(), before.project().name());
    assert_eq!(after.project().settings(), before.project().settings());
    assert_eq!(
        after
            .project()
            .inputs()
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        before
            .project()
            .inputs()
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().input_audio_strips(),
        before.project().input_audio_strips()
    );
    assert_eq!(after.project().scenes(), before.project().scenes());
    assert_eq!(
        after.project().audio_buses(),
        before.project().audio_buses()
    );
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    assert_eq!(after.project().stingers(), before.project().stingers());
    assert_eq!(
        after.project().restart_policy(),
        before.project().restart_policy()
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());

    let unchanged = manifest(&context.project);
    let store = ProjectStore::new(&context.project).unwrap();
    let journal_before = journal_bytes(&store);
    assert_failure_contains(
        &invoke_bounded(&[
            "input-replace-solid",
            context.project_path(),
            "999",
            "1",
            "127",
            "254",
            "0",
        ]),
        "unknown input 999",
    );
    assert_eq!(manifest(&context.project), unchanged);
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_input_replace_media_preserves_identity_and_runtime() {
    let context = ContractContext::new();
    assert_failure_contains(
        &invoke_bounded(&[
            "input-replace-media",
            context.project_path(),
            "2",
            "not-an-asset",
        ]),
        "invalid project asset URI",
    );
    assert!(!context.project.exists());
    assert_success(&invoke_bounded(&["new", context.project_path()]));
    assert_success(&invoke_bounded(&[
        "tbar-start",
        context.project_path(),
        "fade",
        "--key",
        "replace-media-runtime",
        "--expect",
        "0",
    ]));
    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    assert!(before.runtime_manual_transitions().desired.is_some());
    assert!(before.position().revision > 0);
    assert_ne!(before.idempotency_receipts().len(), 0);
    let input = InputId::new(NonZeroU128::new(2).unwrap());
    let before_input = before
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();

    assert_success(&invoke_bounded(&[
        "input-replace-media",
        context.project_path(),
        "2",
        "asset://clip.ppm",
    ]));
    let after = store.load().unwrap();
    let after_input = after
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert_eq!(after_input.id, before_input.id);
    assert_eq!(after_input.name, before_input.name);
    assert_eq!(
        after_input.kind,
        InputKind::Media {
            asset_uri: "asset://clip.ppm".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        }
    );
    assert_eq!(after_input.required_capabilities.len(), 0);
    assert_eq!(
        after.project().schema_version(),
        before.project().schema_version()
    );
    assert_eq!(after.project().id(), before.project().id());
    assert_eq!(after.project().name(), before.project().name());
    assert_eq!(after.project().settings(), before.project().settings());
    assert_eq!(
        after
            .project()
            .inputs()
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        before
            .project()
            .inputs()
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after
            .project()
            .inputs()
            .iter()
            .filter(|candidate| candidate.id != input)
            .collect::<Vec<_>>(),
        before
            .project()
            .inputs()
            .iter()
            .filter(|candidate| candidate.id != input)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().input_audio_strips(),
        before.project().input_audio_strips()
    );
    assert_eq!(after.project().scenes(), before.project().scenes());
    assert_eq!(
        after.project().audio_buses(),
        before.project().audio_buses()
    );
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    assert_eq!(after.project().stingers(), before.project().stingers());
    assert_eq!(
        after.project().restart_policy(),
        before.project().restart_policy()
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());

    let manifest = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    assert_failure_contains(
        &invoke_bounded(&[
            "input-replace-media",
            context.project_path(),
            "999",
            "asset://clip.ppm",
        ]),
        "unknown input 999",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn local_input_replace_scene_preserves_identity_and_rejects_cycle() {
    let context = ContractContext::new();
    let path = context.project_path();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));
    run(&["new", path]);
    run(&[
        "audio-strip",
        path,
        "2",
        "-1200",
        "2500",
        "on",
        "off",
        "off",
        "480",
    ]);
    run(&[
        "tbar-start",
        path,
        "fade",
        "--key",
        "replace-scene",
        "--expect",
        "1",
    ]);
    run(&["scene-input-add", path, "3", "7", "Source"]);

    let store = ProjectStore::new(&context.project).unwrap();
    let before = store.load().unwrap();
    let input = InputId::new(NonZeroU128::new(2).unwrap());
    let source = SceneId::new(NonZeroU128::new(7).unwrap());
    let before_input = before
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();

    run(&["input-replace-scene", path, "2", "7"]);
    let inventory = invoke_bounded(&["inputs", path]);
    assert_success(&inventory);
    assert!(stdout(&inventory).lines().any(|line| {
        line == "input id=2 name=\"Input 2\" kind=scene scene_id=7 audio_source=none strip=gain_mdb=-1200,balance_bp=2500,delay_samples=480,muted=true,soloed=false,follow_video=false"
    }));
    let after = store.load().unwrap();
    let after_input = after
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .unwrap();
    assert_eq!(after_input.id, before_input.id);
    assert_eq!(after_input.name, before_input.name);
    assert_eq!(after_input.required_capabilities, Vec::<String>::new());
    assert_eq!(
        after_input.kind,
        InputKind::Scene {
            scene_id: source,
            audio_source: None,
        }
    );
    assert_eq!(
        after.project().schema_version(),
        before.project().schema_version()
    );
    assert_eq!(after.project().id(), before.project().id());
    assert_eq!(after.project().name(), before.project().name());
    assert_eq!(after.project().settings(), before.project().settings());
    assert_eq!(
        after
            .project()
            .inputs()
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        before
            .project()
            .inputs()
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.project().input_audio_strips(),
        before.project().input_audio_strips()
    );
    assert_eq!(after.project().scenes(), before.project().scenes());
    assert_eq!(
        after.project().audio_buses(),
        before.project().audio_buses()
    );
    assert_eq!(after.project().outputs(), before.project().outputs());
    assert_eq!(after.project().main_mix(), before.project().main_mix());
    assert_eq!(after.project().stingers(), before.project().stingers());
    assert_eq!(
        after.project().restart_policy(),
        before.project().restart_policy()
    );
    assert_eq!(after.runtime_routing(), before.runtime_routing());
    assert_eq!(
        after.runtime_manual_transitions(),
        before.runtime_manual_transitions()
    );
    assert_eq!(
        after.runtime_fade_to_black(),
        before.runtime_fade_to_black()
    );
    assert_eq!(after.runtime_overlays(), before.runtime_overlays());
    assert_eq!(after.position(), before.position());
    assert_eq!(after.idempotency_receipts(), before.idempotency_receipts());

    run(&["scene-input-add", path, "4", "8", "Cycle"]);
    run(&["scene-layer-add", path, "8", "2", "0", "Self reference"]);
    let manifest_before = fs::read(context.project.join("project.json")).unwrap();
    let journal_before = journal_bytes(&store);
    assert_failure_contains(
        &invoke_bounded(&["input-replace-scene", path, "2", "8"]),
        "input source would create a cycle",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);

    // A scene source that does not exist is refused specifically too.
    assert_failure_contains(
        &invoke_bounded(&["input-replace-scene", path, "2", "99"]),
        "unknown scene 99",
    );
    assert_eq!(
        fs::read(context.project.join("project.json")).unwrap(),
        manifest_before
    );
    assert_eq!(journal_bytes(&store), journal_before);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_input_remove_rejects_runtime_reference_and_removes_unused_input() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));
    assert_success(&invoke(&[
        "input-add",
        context.project_path(),
        "3",
        "Unused input",
    ]));
    assert_success(&invoke(&["input-remove", context.project_path(), "3"]));

    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let removed = InputId::new(NonZeroU128::new(3).unwrap());
    assert!(
        !stored
            .project()
            .inputs()
            .iter()
            .any(|input| input.id == removed)
    );
    assert!(stored.project().input_audio_strip(removed).is_none());

    let after_removal = manifest(&context.project);
    let unknown = invoke(&["input-remove", context.project_path(), "3"]);
    assert_failure_contains(&unknown, "unknown input 3");
    assert_eq!(manifest(&context.project), after_removal);

    let routed = invoke(&["input-remove", context.project_path(), "1"]);
    assert_failure_contains(&routed, "runtime reference");
    assert_eq!(manifest(&context.project), after_removal);
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_manual_t_bar_holds_reverses_cancels_commits_and_survives_each_restart() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let start_wipe = invoke(&[
        "tbar-start",
        context.project_path(),
        "wipe",
        "--key",
        "manual-wipe-start",
        "--expect",
        "0",
    ]);
    assert_success(&start_wipe);
    assert!(stdout(&start_wipe).contains("TBar(desired=wipe:1->2@0, realized=wipe:1->2@0)"));
    assert_eq!(status(&context.project), stdout(&start_wipe));

    let endpoint = invoke(&[
        "tbar-position",
        context.project_path(),
        "10000",
        "--key",
        "manual-end",
        "--expect",
        "1",
    ]);
    assert_success(&endpoint);
    assert!(stdout(&endpoint).contains("TBar(desired=wipe:1->2@10000, realized=wipe:1->2@10000)"));
    assert_eq!(status(&context.project), stdout(&endpoint));
    let endpoint_manifest = manifest(&context.project);
    assert!(endpoint_manifest.contains(
        r#""desired": {"kind": "wipe", "from_id": 1, "to_id": 2, "interval_start_basis_points": 0, "position_basis_points": 10000}"#
    ));
    assert!(endpoint_manifest.contains(
        r#""realized": {"kind": "wipe", "from_id": 1, "to_id": 2, "interval_start_basis_points": 10000, "position_basis_points": 10000}"#
    ));

    let reversed = invoke(&[
        "tbar-position",
        context.project_path(),
        "2500",
        "--key",
        "manual-reverse",
        "--expect",
        "2",
    ]);
    assert_success(&reversed);
    assert!(stdout(&reversed).contains("TBar(desired=wipe:1->2@2500, realized=wipe:1->2@2500)"));
    assert_eq!(status(&context.project), stdout(&reversed));

    let cancelled = invoke(&[
        "tbar-cancel",
        context.project_path(),
        "--key",
        "manual-cancel",
        "--expect",
        "3",
    ]);
    assert_success(&cancelled);
    assert_status(&stdout(&cancelled), 4, 4, 1, 1, 2, 2);
    assert!(stdout(&cancelled).contains("TBar(desired=inactive, realized=inactive)"));

    for (command, revision, key) in [
        ("tbar-start", "4", "manual-fade-start"),
        ("tbar-position", "5", "manual-fade-end"),
    ] {
        let value = if command == "tbar-start" {
            "fade"
        } else {
            "10000"
        };
        let output = invoke(&[
            command,
            context.project_path(),
            value,
            "--key",
            key,
            "--expect",
            revision,
        ]);
        assert_success(&output);
    }
    let committed = invoke(&[
        "tbar-commit",
        context.project_path(),
        "--key",
        "manual-commit",
        "--expect",
        "6",
    ]);
    assert_success(&committed);
    assert_status(&stdout(&committed), 7, 7, 2, 2, 1, 1);
    assert!(stdout(&committed).contains("TBar(desired=inactive, realized=inactive)"));
    assert_eq!(status(&context.project), stdout(&committed));

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_manual_alpha_fade_survives_restart_and_replays_idempotently() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let started = invoke(&[
        "tbar-start",
        context.project_path(),
        "alpha-fade",
        "--key",
        "manual-alpha-start",
        "--expect",
        "0",
    ]);
    assert_success(&started);
    assert!(
        stdout(&started).contains("TBar(desired=alpha_fade:1->2@0, realized=alpha_fade:1->2@0)")
    );
    assert_eq!(status(&context.project), stdout(&started));

    let positioned = invoke(&[
        "tbar-position",
        context.project_path(),
        "6250",
        "--key",
        "manual-alpha-position",
        "--expect",
        "1",
    ]);
    assert_success(&positioned);
    let positioned_status = stdout(&positioned);
    assert!(
        positioned_status
            .contains("TBar(desired=alpha_fade:1->2@6250, realized=alpha_fade:1->2@6250)")
    );
    assert_eq!(status(&context.project), positioned_status);
    let positioned_manifest = manifest(&context.project);
    assert!(positioned_manifest.contains(
        r#""desired": {"kind": "alpha_fade", "from_id": 1, "to_id": 2, "interval_start_basis_points": 0, "position_basis_points": 6250}"#
    ));
    assert!(positioned_manifest.contains(
        r#""realized": {"kind": "alpha_fade", "from_id": 1, "to_id": 2, "interval_start_basis_points": 6250, "position_basis_points": 6250}"#
    ));

    let repeated = invoke(&[
        "tbar-position",
        context.project_path(),
        "9000",
        "--key",
        "manual-alpha-position",
        "--expect",
        "0",
    ]);
    assert_success(&repeated);
    assert_eq!(stdout(&repeated), positioned_status);
    assert_eq!(manifest(&context.project), positioned_manifest);

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn local_manual_slide_survives_restart_with_exact_desired_and_realized_state() {
    let context = ContractContext::new();
    assert_success(&invoke(&["new", context.project_path()]));

    let started = invoke(&[
        "tbar-start",
        context.project_path(),
        "slide",
        "--key",
        "manual-slide-start",
        "--expect",
        "0",
    ]);
    assert_success(&started);
    assert!(stdout(&started).contains("TBar(desired=slide:1->2@0, realized=slide:1->2@0)"));

    let positioned = invoke(&[
        "tbar-position",
        context.project_path(),
        "6250",
        "--key",
        "manual-slide-position",
        "--expect",
        "1",
    ]);
    assert_success(&positioned);
    assert!(
        stdout(&positioned).contains("TBar(desired=slide:1->2@6250, realized=slide:1->2@6250)")
    );
    assert_eq!(status(&context.project), stdout(&positioned));
    let stored = manifest(&context.project);
    assert!(stored.contains(
        r#""desired": {"kind": "slide", "from_id": 1, "to_id": 2, "interval_start_basis_points": 0, "position_basis_points": 6250}"#
    ));
    assert!(stored.contains(
        r#""realized": {"kind": "slide", "from_id": 1, "to_id": 2, "interval_start_basis_points": 6250, "position_basis_points": 6250}"#
    ));

    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn cli_rejects_fractional_and_out_of_range_t_bar_positions_before_project_io() {
    let fractional = invoke(&["tbar-position", "missing.freemix", "62.5"]);
    assert_failure_contains(&fractional, "invalid basis points value `62.5`");
    let out_of_range = invoke(&["tbar-position", "missing.freemix", "10001"]);
    assert_failure_contains(
        &out_of_range,
        "basis points must be in 0..=10000, got 10001",
    );
}

struct ContractContext {
    root: PathBuf,
    project: PathBuf,
    first_image: PathBuf,
    second_image: PathBuf,
    final_image: PathBuf,
}

impl ContractContext {
    fn new() -> Self {
        let root = unique_test_root();
        fs::create_dir_all(&root).unwrap();
        Self {
            project: root.join("contract.freemix"),
            first_image: root.join("first.ppm"),
            second_image: root.join("second.ppm"),
            final_image: root.join("final.ppm"),
            root,
        }
    }

    fn project_path(&self) -> &str {
        self.project.to_str().unwrap()
    }
}

fn assert_contract_creation(context: &ContractContext) -> (u128, String) {
    let created = invoke(&["new", context.project_path(), "--name", "Contract Show"]);
    assert_success(&created);
    let created_status = stdout(&created);
    let project_id = status_u128(&created_status, "project_id");
    assert!(project_id > u128::from(u64::MAX));
    assert_status(&created_status, 0, 0, 1, 1, 2, 2);
    (project_id, created_status)
}

fn assert_initial_contract_state(
    context: &ContractContext,
    project_id: u128,
    created_status: &str,
) {
    let initial_status = status(&context.project);
    assert_eq!(initial_status, created_status);
    let initial_manifest = manifest(&context.project);
    assert_eq!(json_u128(&initial_manifest, "id"), project_id);
    assert_eq!(json_number(&initial_manifest, "frames_rendered"), 0);
    assert_eq!(json_number(&initial_manifest, "runtime_generation"), 0);
    assert_eq!(json_number(&initial_manifest, "clock_time_nanos"), 0);
}

fn assert_contract_render(
    context: &ContractContext,
    image: &Path,
    width: u32,
    height: u32,
    rgb: [u8; 3],
) {
    fs::write(image, b"previous complete PPM").unwrap();
    assert_success(&invoke(&[
        "render",
        context.project_path(),
        image.to_str().unwrap(),
        "--width",
        &width.to_string(),
        "--height",
        &height.to_string(),
    ]));
    assert_solid_ppm(image, width, height, rgb);
    assert_eq!(
        fs::read_dir(image.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count(),
        0
    );
}

fn select_contract_preview(context: &ContractContext) {
    let preview = invoke(&[
        "preview",
        context.project_path(),
        "2",
        "--key",
        "preview-two",
        "--expect",
        "0",
    ]);
    assert_success(&preview);
    let after_preview = stdout(&preview);
    assert_status(&after_preview, 1, 1, 1, 1, 2, 2);
}

fn assert_contract_rejections(context: &ContractContext) {
    let before_invalid = status(&context.project);
    let invalid = invoke(&[
        "preview",
        context.project_path(),
        "99",
        "--key",
        "invalid-input",
        "--expect",
        "1",
    ]);
    assert_failure_contains(&invalid, "not_found");
    assert_eq!(status(&context.project), before_invalid);
    let invalid_manifest = manifest(&context.project);
    assert!(invalid_manifest.contains("\"key\": \"invalid-input\""));
    assert!(invalid_manifest.contains("\"code\": \"not_found\""));

    let invalid_replay = invoke(&[
        "preview",
        context.project_path(),
        "1",
        "--key",
        "invalid-input",
        "--expect",
        "1",
    ]);
    assert_failure_contains(&invalid_replay, "not_found");
    assert_eq!(manifest(&context.project), invalid_manifest);

    let stale = invoke(&[
        "cut",
        context.project_path(),
        "--key",
        "stale-cut",
        "--expect",
        "0",
    ]);
    assert_failure_contains(&stale, "revision_conflict");
    assert_eq!(status(&context.project), before_invalid);
    let stale_manifest = manifest(&context.project);
    assert!(stale_manifest.contains("\"key\": \"stale-cut\""));
    assert!(stale_manifest.contains("\"code\": \"revision_conflict\""));
    assert_eq!(json_number(&stale_manifest, "revision"), 1);
    assert_eq!(json_number(&stale_manifest, "frames_rendered"), 1);

    let stale_replay = invoke(&[
        "cut",
        context.project_path(),
        "--key",
        "stale-cut",
        "--expect",
        "1",
    ]);
    assert_failure_contains(&stale_replay, "revision_conflict");
    assert_eq!(manifest(&context.project), stale_manifest);
}

fn cut_contract(context: &ContractContext) -> (String, String) {
    let cut = invoke(&[
        "cut",
        context.project_path(),
        "--key",
        "cut-one",
        "--expect",
        "1",
    ]);
    assert_success(&cut);
    let after_cut = stdout(&cut);
    assert_status(&after_cut, 2, 2, 2, 2, 1, 1);
    let cut_manifest = manifest(&context.project);
    assert_eq!(json_number(&cut_manifest, "runtime_generation"), 2);
    assert!(json_number(&cut_manifest, "clock_time_nanos") > 0);
    (after_cut, cut_manifest)
}

fn assert_duplicate_cut_replay(
    context: &ContractContext,
    expected_status: &str,
    cut_manifest: &str,
) {
    let duplicate = invoke(&[
        "fade",
        context.project_path(),
        "99",
        "--key",
        "cut-one",
        "--expect",
        "0",
    ]);
    assert_success(&duplicate);
    assert_eq!(stdout(&duplicate), expected_status);
    assert_eq!(manifest(&context.project), cut_manifest);
}

fn fade_and_assert_final_contract(context: &ContractContext, project_id: u128) {
    let fade = invoke(&[
        "fade",
        context.project_path(),
        "4",
        "--key",
        "fade-four",
        "--expect",
        "2",
    ]);
    assert_success(&fade);
    let after_fade = stdout(&fade);
    assert_status(&after_fade, 3, 6, 1, 1, 2, 2);
    assert_eq!(status(&context.project), after_fade);

    let final_manifest = manifest(&context.project);
    assert_eq!(json_u128(&final_manifest, "id"), project_id);
    assert_eq!(json_number(&final_manifest, "revision"), 3);
    assert_eq!(json_number(&final_manifest, "event_sequence"), 3);
    assert_eq!(json_number(&final_manifest, "frames_rendered"), 6);
    assert_eq!(json_number(&final_manifest, "runtime_generation"), 3);
    assert!(json_number(&final_manifest, "clock_time_nanos") > 0);
    assert!(final_manifest.contains("\"target_frame\": 0"));
    assert!(final_manifest.contains("\"target_frame\": 1"));
    assert!(final_manifest.contains("\"target_frame\": 2"));

    let entries: Vec<_> = fs::read_dir(&context.project)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, ["project.json"]);

    assert_contract_render(context, &context.final_image, 2, 1, [73, 151, 199]);
}

#[test]
fn demo_persists_one_nonzero_identity_through_reload() {
    let root = unique_test_root();
    let project = root.join("demo.freemix");
    fs::create_dir_all(&root).unwrap();

    let demo = invoke(&["demo", project.to_str().unwrap()]);
    assert_success(&demo);
    let output = stdout(&demo);
    let statuses: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("project_id="))
        .collect();
    assert_eq!(statuses.len(), 4);
    let project_id = status_u128(statuses[0], "project_id");
    assert_ne!(project_id, 0);
    assert!(
        statuses
            .iter()
            .all(|line| { status_u128(line, "project_id") == project_id })
    );
    assert_eq!(statuses[2], statuses[3]);
    assert_status(statuses[3], 2, 5, 1, 1, 2, 2);
    assert_eq!(status(&project), statuses[3]);
    assert_eq!(json_u128(&manifest(&project), "id"), project_id);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_refuses_to_replace_an_existing_bundle() {
    let root = unique_test_root();
    let project = root.join("existing.freemix");
    fs::create_dir_all(&root).unwrap();
    let project_path = project.to_str().unwrap();

    let created = invoke(&["new", project_path, "--name", "Original Show"]);
    assert_success(&created);
    let original_status = stdout(&created);
    let original_manifest = manifest(&project);
    let original_id = status_u128(&original_status, "project_id");

    let replacement = invoke(&["new", project_path, "--name", "Replacement Show"]);
    assert_failure_contains(&replacement, "already exists");
    assert_eq!(status_u128(&status(&project), "project_id"), original_id);
    assert_eq!(status(&project), original_status);
    assert_eq!(manifest(&project), original_manifest);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_keys_do_not_replay_a_rejection_for_a_corrected_command() {
    let root = unique_test_root();
    let project = root.join("corrected.freemix");
    fs::create_dir_all(&root).unwrap();
    let project_path = project.to_str().unwrap();
    assert_success(&invoke(&["new", project_path]));

    let invalid = invoke(&["preview", project_path, "99"]);
    assert_failure_contains(&invalid, "not_found");
    assert_eq!(json_number(&manifest(&project), "revision"), 0);

    let corrected = invoke(&["preview", project_path, "2"]);
    assert_success(&corrected);
    assert_status(&stdout(&corrected), 1, 1, 1, 1, 2, 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn blank_explicit_key_fails_without_mutating_the_project() {
    let root = unique_test_root();
    let project = root.join("blank-key.freemix");
    fs::create_dir_all(&root).unwrap();
    let project_path = project.to_str().unwrap();
    assert_success(&invoke(&["new", project_path]));
    let before = manifest(&project);

    let blank = invoke(&["cut", project_path, "--key", "  \t"]);
    assert_failure_contains(&blank, "idempotency key must not be blank");
    assert_eq!(manifest(&project), before);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn status_escapes_show_names_onto_one_line() {
    let root = unique_test_root();
    let project = root.join("escaped.freemix");
    fs::create_dir_all(&root).unwrap();

    let created = invoke(&[
        "new",
        project.to_str().unwrap(),
        "--name",
        "First line\n\"Second line\"",
    ]);
    assert_success(&created);
    let output = stdout(&created);
    assert_eq!(output.lines().count(), 1);
    assert!(output.contains(r#"show="First line\n\"Second line\"""#));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn edited_current_configuration_survives_commands_restart_and_render() {
    let root = unique_test_root();
    let project = root.join("edited.freemix");
    let solid_image = root.join("solid.ppm");
    let bars_image = root.join("bars.ppm");
    fs::create_dir_all(&root).unwrap();
    let project_path = project.to_str().unwrap();
    assert_success(&invoke(&["new", project_path, "--name", "Edited V3"]));

    edit_current_configuration(&project);

    let before = status(&project);
    assert_status(&before, 0, 0, 1, 1, 2, 2);
    assert_success(&invoke(&[
        "render",
        project_path,
        solid_image.to_str().unwrap(),
        "--width",
        "3",
        "--height",
        "2",
    ]));
    assert_solid_ppm(&solid_image, 3, 2, [12, 34, 56]);

    let cut = invoke(&["cut", project_path, "--key", "edited-cut", "--expect", "0"]);
    assert_success(&cut);
    let after_cut = stdout(&cut);
    assert_eq!(status(&project), after_cut);
    assert_status(&after_cut, 1, 1, 2, 2, 1, 1);
    assert_edited_configuration(&manifest(&project));

    assert_success(&invoke(&[
        "render",
        project_path,
        bars_image.to_str().unwrap(),
        "--width",
        "8",
        "--height",
        "2",
    ]));
    assert_ppm_top_row(
        &bars_image,
        8,
        2,
        &[
            [191, 191, 191],
            [191, 191, 0],
            [0, 191, 191],
            [0, 191, 0],
            [191, 0, 191],
            [191, 0, 0],
            [0, 0, 191],
            [0, 0, 0],
        ],
    );

    fs::remove_dir_all(root).unwrap();
}

fn edit_current_configuration(project: &Path) {
    let mut source = manifest(project);
    let default_rate = r#"{"numerator": 60000, "denominator": 1001}"#;
    assert_eq!(source.matches(default_rate).count(), 2);
    source = source.replace(default_rate, r#"{"numerator": 24000, "denominator": 1001}"#);
    replace_once(&mut source, r#""width": 1920"#, r#""width": 1280"#);
    replace_once(&mut source, r#""height": 1080"#, r#""height": 720"#);
    replace_once(
        &mut source,
        r#""pixel_format": "nv12""#,
        r#""pixel_format": "rgba8""#,
    );
    replace_once(
        &mut source,
        r#""sample_rate_hz": 48000"#,
        r#""sample_rate_hz": 96000"#,
    );
    replace_once(
        &mut source,
        r#""sample_format": "f32""#,
        r#""sample_format": "i24""#,
    );
    replace_once(
        &mut source,
        r#""video": {"type": "solid", "red": 73, "green": 151, "blue": 199, "alpha": 255}, "audio": {"type": "silence"}"#,
        r#""video": {"type": "solid", "red": 12, "green": 34, "blue": 56, "alpha": 128}, "audio": {"type": "sine", "frequency_hz": 777}"#,
    );
    replace_once(
        &mut source,
        r#""video": {"type": "solid", "red": 146, "green": 46, "blue": 142, "alpha": 255}, "audio": {"type": "sine", "frequency_hz": 1000}"#,
        r#""video": {"type": "bars"}, "audio": {"type": "silence"}"#,
    );
    replace_once(
        &mut source,
        r#""required_capabilities": []"#,
        r#""required_capabilities": ["sim.input.edited"]"#,
    );
    replace_once(
        &mut source,
        r#""scenes": []"#,
        r#""scenes": [{"id": 11, "name": "Edited scene", "background": {"red": 0, "green": 0, "blue": 0, "alpha": 255}, "layers": [{"id": 1, "name": "Source", "source": {"type": "input", "id": 1}, "enabled": true, "geometry": {"translation_x": 0, "translation_y": 0, "width": 1280, "height": 720, "rotation": "deg0"}, "crop": null, "mask": null, "effects": [], "opacity": 255, "z_order": 0}], "animation": {"timebase": "30/1", "revision": 0, "in_frames": 0, "out_frames": 0, "tracks": []}}]"#,
    );
    replace_once(
        &mut source,
        r#""audio_buses": []"#,
        r#""audio_buses": [{"id": 21, "name": "Edited bus", "sends": []}]"#,
    );
    replace_once(
        &mut source,
        r#""outputs": []"#,
        r#""outputs": [{"id": 31, "name": "Edited output", "video_source": 11, "audio_source": 21, "startup": "reconcile_desired_state", "required_capabilities": ["output.edited"]}]"#,
    );
    replace_once(
        &mut source,
        r#""restart_policy": {"type": "never"}"#,
        r#""restart_policy": {"type": "on_failure", "max_attempts": 7}"#,
    );
    fs::write(project.join("project.json"), source).unwrap();
}

fn assert_edited_configuration(saved: &str) {
    for expected in [
        r#"{"numerator": 24000, "denominator": 1001}"#,
        r#""width": 1280"#,
        r#""height": 720"#,
        r#""pixel_format": "rgba8""#,
        r#""sample_rate_hz": 96000"#,
        r#""sample_format": "i24""#,
        r#""red": 12, "green": 34, "blue": 56, "alpha": 128"#,
        r#""frequency_hz": 777"#,
        r#""video": {"type": "bars"}, "audio": {"type": "silence"}"#,
        r#""sim.input.edited""#,
        r#""Edited scene""#,
        r#""Edited bus""#,
        r#""Edited output""#,
        r#""output.edited""#,
        r#""type": "on_failure", "max_attempts": 7"#,
    ] {
        assert!(saved.contains(expected), "missing persisted `{expected}`");
    }
    assert_eq!(
        saved
            .matches(r#"{"numerator": 24000, "denominator": 1001}"#)
            .count(),
        2
    );
}

#[test]
fn render_rejects_non_simulated_inputs_clearly() {
    let root = unique_test_root();
    let project = root.join("unsupported.freemix");
    let image = root.join("unsupported.ppm");
    fs::create_dir_all(&root).unwrap();
    assert_success(&invoke(&["new", project.to_str().unwrap()]));

    let mut source = manifest(&project);
    replace_once(
        &mut source,
        r#"{"type": "simulated", "video": {"type": "solid", "red": 73, "green": 151, "blue": 199, "alpha": 255}, "audio": {"type": "silence"}}"#,
        r#"{"type": "color"}"#,
    );
    fs::write(project.join("project.json"), source).unwrap();

    let rendered = invoke(&["render", project.to_str().unwrap(), image.to_str().unwrap()]);
    assert_failure_contains(
        &rendered,
        "input 1 (\"Input 1\") is not simulated; render supports only simulated inputs",
    );
    assert!(!image.exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_scene_program_render_composes_supported_layers_and_rejects_unsupported_states() {
    let root = unique_test_root();
    let project = root.join("scene.freemix");
    let image = root.join("scene.ppm");
    let nested_image = root.join("nested.ppm");
    let transition_image = root.join("transition.ppm");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    macro_rules! apply {
        ($arguments:expr) => {
            assert_success(&invoke_bounded($arguments));
        };
    }

    apply!(&["new", path]);
    // Scene geometry is authored in project pixels; a 4x4 project makes the
    // 4x4 render a 1:1 view of the authored rectangles.
    let mut source = manifest(&project);
    replace_once(&mut source, "\"width\": 1920,", "\"width\": 4,");
    replace_once(&mut source, "\"height\": 1080,", "\"height\": 4,");
    fs::write(project.join("project.json"), source).unwrap();
    apply!(&["input-replace-simulated", path, "2"]);
    apply!(&["scene-input-add", path, "3", "11", "Program Scene"]);
    apply!(&["scene-background", path, "11", "10", "20", "30", "255"]);
    apply!(&["scene-layer-add", path, "11", "1", "5", "Solid"]);
    apply!(&[
        "scene-layer-geometry",
        path,
        "11",
        "0",
        "1",
        "1",
        "2",
        "2",
        "0",
    ]);
    apply!(&["scene-layer-add", path, "11", "2", "5", "Bars"]);
    apply!(&["scene-layer-appearance", path, "11", "1", "on", "128"]);
    apply!(&[
        "scene-layer-geometry",
        path,
        "11",
        "1",
        "1",
        "1",
        "2",
        "2",
        "90",
    ]);
    apply!(&["scene-layer-crop", path, "11", "1", "0", "0", "2", "2"]);
    apply!(&[
        "scene-layer-mask",
        path,
        "11",
        "1",
        "0",
        "0",
        "1",
        "1",
        "inverted",
    ]);
    apply!(&["preview", path, "3"]);
    apply!(&["cut", path]);
    apply!(&[
        "render",
        path,
        image.to_str().unwrap(),
        "--width",
        "4",
        "--height",
        "4",
    ]);

    let bytes = fs::read(&image).unwrap();
    let header = b"P6\n4 4\n255\n";
    assert!(bytes.starts_with(header));
    assert_eq!(
        &bytes[header.len()..],
        &[
            10, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30, 132, 171, 195, 73, 151,
            199, 10, 20, 30, 10, 20, 30, 36, 171, 195, 36, 171, 195, 10, 20, 30, 10, 20, 30, 10,
            20, 30, 10, 20, 30, 10, 20, 30,
        ]
    );

    apply!(&["scene-input-add", path, "4", "12", "Nested"]);
    apply!(&[
        "scene-layer-add-scene",
        path,
        "11",
        "12",
        "6",
        "Nested layer",
    ]);
    let nested = invoke_bounded(&["render", path, nested_image.to_str().unwrap()]);
    assert_failure_contains(&nested, "scene source 12 is unsupported");
    assert!(!nested_image.exists());

    apply!(&["preview", path, "1"]);
    apply!(&["tbar-start", path, "fade"]);
    let transition = invoke_bounded(&["render", path, transition_image.to_str().unwrap()]);
    assert_failure_contains(
        &transition,
        "render does not support transitions with Scene input endpoints",
    );
    assert!(!transition_image.exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn journal_recover_reports_a_clean_journal_and_refuses_unapplied_batches() {
    let root = unique_test_root();
    let clean_project = root.join("clean.freemix");
    let unapplied_project = root.join("unapplied.freemix");
    fs::create_dir_all(&root).unwrap();

    // A journal with nothing left to apply recovers to its durable checkpoint.
    assert_success(&invoke(&["new", clean_project.to_str().unwrap()]));
    let recovered = invoke(&["journal-recover", clean_project.to_str().unwrap()]);
    assert_success(&recovered);
    assert_eq!(
        String::from_utf8_lossy(&recovered.stdout),
        "journal recovered: checkpoint_sequence=0 checkpoint_revision=0 unapplied_batches=0\n"
    );
    assert_success(&invoke(&["status", clean_project.to_str().unwrap()]));

    // Batches the CLI cannot interpret are refused, and nothing is applied.
    assert_success(&invoke(&["new", unapplied_project.to_str().unwrap()]));
    let store = ProjectStore::new(&unapplied_project).unwrap();
    store
        .append_batch(&MutationBatch::new(1, 0, 1, b"valid".to_vec()))
        .unwrap();
    let manifest = fs::read(unapplied_project.join("project.json")).unwrap();

    let refused = invoke(&["journal-recover", unapplied_project.to_str().unwrap()]);
    assert_failure_contains(
        &refused,
        "journal recovery refused: 1 unapplied journal batch(es) remain; no batches were applied",
    );
    let scan = store.scan_journal().unwrap();
    assert_eq!(scan.batches().len(), 1);
    assert_eq!(scan.batches()[0].payload(), b"valid");
    assert_eq!(
        fs::read(unapplied_project.join("project.json")).unwrap(),
        manifest
    );
    fs::remove_dir_all(root).unwrap();
}

/// turso locks the journal database exclusively even to read it, so a process
/// that opens it locks every other process out. Inspecting a project must
/// therefore never open it: `status` has to work on a show whose journal is
/// held by a running daemon, and concurrent readers must not lock each other
/// out. The command that does own the journal reports the problem instead.
#[test]
fn status_reads_a_project_without_opening_its_journal() {
    let root = unique_test_root();
    fs::create_dir_all(&root).unwrap();
    let path = root.join("show.freemix");
    let project = path.to_str().unwrap();
    assert_success(&invoke(&["new", project]));
    assert_success(&invoke(&["journal-recover", project]));
    assert!(path.join("journal").join("journal.db").is_file());

    let concurrent: Vec<_> = (0..12)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_freemix-cli"))
                .args(["status", project])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for reader in concurrent {
        assert_success(&reader.wait_with_output().unwrap());
    }

    // A journal database that cannot be opened at all proves the point: the
    // manifest still reports, and only the journal command fails.
    let database = path.join("journal").join("journal.db");
    fs::remove_file(&database).unwrap();
    fs::create_dir(&database).unwrap();
    assert_success(&invoke(&["status", project]));
    let refused = invoke(&["journal-recover", project]);
    assert!(
        !refused.status.success(),
        "recovery must not report a clean journal it could not open"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_overlay_next_runs_a_fade_transition_to_completion() {
    let root = unique_test_root();
    let project = root.join("overlay-next.freemix");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();

    assert_success(&invoke_bounded(&["new", path]));
    assert_success(&invoke_bounded(&[
        "overlay-transition",
        path,
        "1",
        "fade",
        "10",
    ]));
    assert_success(&invoke_bounded(&["overlay-queue", path, "1", "2"]));
    let next = invoke_bounded(&["overlay-next", path, "1"]);
    assert_success(&next);
    let reported = stdout(&next);
    assert!(
        reported.contains("realized=[1:2:on:opacity=255:fade@10"),
        "overlay-next did not realize the faded overlay: {reported}"
    );
    assert_eq!(status(&project), reported);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_scene_render_maps_project_coordinates_onto_the_output_size() {
    let root = unique_test_root();
    let project = root.join("coordinates.freemix");
    let quarter = root.join("quarter.ppm");
    let cropped = root.join("cropped.ppm");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    let run = |arguments: &[&str]| assert_success(&invoke_bounded(arguments));

    run(&["new", path]);
    run(&["input-replace-solid", path, "1", "200", "100", "50", "255"]);
    run(&["scene-input-add", path, "3", "10", "Cam"]);
    run(&["scene-background", path, "10", "10", "20", "30", "255"]);
    run(&["scene-layer-add", path, "10", "1", "0", "L1"]);
    // The default project is 1920x1080, so this is the top-left quarter.
    run(&[
        "scene-layer-geometry",
        path,
        "10",
        "0",
        "0",
        "0",
        "960",
        "540",
        "0",
    ]);
    run(&["preview", path, "3"]);
    run(&["cut", path]);
    run(&["render", path, quarter.to_str().unwrap()]);
    let layer = [200, 100, 50];
    let background = [10, 20, 30];
    for (x, y, expected) in [
        (0, 0, layer),
        (319, 179, layer),
        (320, 0, background),
        (0, 180, background),
        (639, 359, background),
    ] {
        assert_eq!(
            ppm_pixel(&quarter, 640, 360, x, y),
            expected,
            "pixel {x},{y}"
        );
    }

    // A crop authored in project space (valid against 1920x1080) renders at
    // the smaller default output instead of being re-checked against it.
    run(&["scene-layer-crop", path, "10", "0", "0", "0", "1000", "500"]);
    run(&["render", path, cropped.to_str().unwrap()]);
    assert_eq!(ppm_pixel(&cropped, 640, 360, 0, 0), layer);
    assert_eq!(ppm_pixel(&cropped, 640, 360, 320, 180), background);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_stream_update_preserves_desired_running() {
    let context = ContractContext::new();
    author_stream_destination(&context);
    assert_success(&invoke_bounded(&[
        "stream-start",
        context.project_path(),
        "40",
    ]));
    assert_success(&invoke_bounded(&[
        "stream-update",
        context.project_path(),
        "40",
        "30",
        "rtmp://relay.example.test/live",
        "rotated-key-0001",
        "Main ingest",
    ]));
    let streams = invoke_bounded(&["streams", context.project_path()]);
    assert_success(&streams);
    assert!(
        stdout(&streams).ends_with("running=true"),
        "stream-update cleared desired running: {}",
        stdout(&streams)
    );
    let stored = ProjectStore::new(&context.project).unwrap().load().unwrap();
    let target = &stored.project().stream_targets()[0];
    assert!(target.running());
    assert_eq!(target.key().unwrap().expose_secret(), "rotated-key-0001");
    fs::remove_dir_all(context.root).unwrap();
}

#[test]
fn demo_refuses_to_overwrite_an_existing_bundle() {
    let root = unique_test_root();
    let project = root.join("existing.freemix");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    assert_success(&invoke_bounded(&["new", path, "--name", "Keep Me"]));
    assert_success(&invoke_bounded(&["cut", path]));
    let before = fs::read(project.join("project.json")).unwrap();

    let refused = invoke_bounded(&["demo", path]);
    assert_failure_contains(&refused, "destination bundle already exists");
    assert_eq!(fs::read(project.join("project.json")).unwrap(), before);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_implicit_idempotency_keys_are_compact() {
    let root = unique_test_root();
    let project = root.join("keys.freemix");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    assert_success(&invoke_bounded(&["new", path]));
    assert_success(&invoke_bounded(&["cut", path]));
    assert_success(&invoke_bounded(&[
        "overlay-transition",
        path,
        "1",
        "fade",
        "3",
    ]));

    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let receipts = stored.idempotency_receipts();
    assert_eq!(receipts.len(), 2);
    for receipt in receipts {
        // `cli:` + 32 hex timestamp + `:` + 8 hex pid + `:` + 16 hex sequence.
        assert_eq!(
            receipt.key().len(),
            62,
            "implicit key is not compact: {}",
            receipt.key()
        );
        assert!(receipt.key().starts_with("cli:"));
        assert!(
            receipt.key()[4..]
                .chars()
                .all(|character| character == ':' || character.is_ascii_hexdigit()),
            "implicit key carries a command payload: {}",
            receipt.key()
        );
    }

    fs::remove_dir_all(root).unwrap();
}

fn ppm_pixel(path: &Path, width: u32, height: u32, x: u32, y: u32) -> [u8; 3] {
    let bytes = fs::read(path).unwrap();
    let header = format!("P6\n{width} {height}\n255\n");
    assert!(bytes.starts_with(header.as_bytes()));
    let offset = header.len() + ((y * width + x) as usize) * 3;
    [bytes[offset], bytes[offset + 1], bytes[offset + 2]]
}

fn invoke(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_freemix-cli"))
        .args(arguments)
        .output()
        .unwrap()
}

fn invoke_bounded(arguments: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemix-cli"))
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + TEST_CLI_TIMEOUT;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI process did not exit before {TEST_CLI_TIMEOUT:?}: status={} stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn replace_once(source: &mut String, from: &str, to: &str) {
    assert!(source.contains(from), "manifest did not contain `{from}`");
    *source = source.replacen(from, to, 1);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "process failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure_contains(output: &Output, expected: &str) {
    assert!(!output.status.success(), "process unexpectedly succeeded");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "stderr did not contain `{expected}`: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_owned()
}

fn status(project: &Path) -> String {
    let output = invoke(&["status", project.to_str().unwrap()]);
    assert_success(&output);
    stdout(&output)
}

fn assert_status(
    status: &str,
    revision: u64,
    frame: u64,
    desired_program: u64,
    realized_program: u64,
    desired_preview: u64,
    realized_preview: u64,
) {
    assert_eq!(status_number(status, "revision"), revision);
    assert_eq!(status_number(status, "frame"), frame);
    assert!(status.contains(&format!(
        "Program(desired={desired_program}, realized={realized_program})"
    )));
    assert!(status.contains(&format!(
        "Preview(desired={desired_preview}, realized={realized_preview})"
    )));
}

fn assert_remote_status(
    status: &str,
    revision: u64,
    desired_program: u64,
    realized_program: u64,
    desired_preview: u64,
    realized_preview: u64,
) {
    assert_eq!(status_number(status, "revision"), revision);
    assert!(status.contains("frame=unavailable"));
    assert!(status.contains(&format!(
        "Program(desired={desired_program}, realized={realized_program})"
    )));
    assert!(status.contains(&format!(
        "Preview(desired={desired_preview}, realized={realized_preview})"
    )));
}

fn status_number(status: &str, field: &str) -> u64 {
    number_after(status, &format!("{field}="))
}

fn status_u128(status: &str, field: &str) -> u128 {
    u128_after(status, &format!("{field}="))
}

fn json_number(source: &str, field: &str) -> u64 {
    number_after(source, &format!("\"{field}\": "))
}

fn json_u128(source: &str, field: &str) -> u128 {
    u128_after(source, &format!("\"{field}\": "))
}

fn number_after(source: &str, prefix: &str) -> u64 {
    u128_after(source, prefix).try_into().unwrap()
}

fn u128_after(source: &str, prefix: &str) -> u128 {
    let start = source.find(prefix).unwrap() + prefix.len();
    let digits: String = source[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

fn manifest(project: &Path) -> String {
    fs::read_to_string(project.join("project.json")).unwrap()
}

fn assert_solid_ppm(path: &Path, width: u32, height: u32, rgb: [u8; 3]) {
    let bytes = fs::read(path).unwrap();
    let header = format!("P6\n{width} {height}\n255\n");
    assert!(bytes.starts_with(header.as_bytes()));
    let pixels = &bytes[header.len()..];
    assert_eq!(pixels.len(), (width * height * 3) as usize);
    assert!(pixels.as_chunks::<3>().0.iter().all(|pixel| *pixel == rgb));
}

fn assert_ppm_top_row(path: &Path, width: u32, height: u32, expected: &[[u8; 3]]) {
    let bytes = fs::read(path).unwrap();
    let header = format!("P6\n{width} {height}\n255\n");
    assert!(bytes.starts_with(header.as_bytes()));
    let top_row = &bytes[header.len()..header.len() + width as usize * 3];
    assert_eq!(top_row.as_chunks::<3>().0, expected);
}

fn unique_test_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = TEST_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "freemix-cli-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}

/// GX-011 stage 5: scene layer animation authored locally and rendered at
/// exact frames of its phases (the e2e golden).
#[test]
#[allow(clippy::too_many_lines)] // one golden: author, render, retime, refuse
fn scene_animation_presets_retime_and_render_exact_frames() {
    let root = unique_test_root();
    let project = root.join("scene-animation.freemix");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    let run = |arguments: &[&str]| {
        let output = invoke_bounded(arguments);
        assert_success(&output);
        stdout(&output)
    };
    run(&["new", path]);
    run(&["input-replace-solid", path, "1", "200", "100", "50", "255"]);
    run(&["scene-input-add", path, "3", "10", "Cam"]);
    run(&["scene-background", path, "10", "10", "20", "30", "255"]);
    run(&["scene-layer-add", path, "10", "1", "0", "L1"]);
    // The top-left quarter of the 1920x1080 canvas; the layer's id is 1.
    run(&[
        "scene-layer-geometry",
        path,
        "10",
        "0",
        "0",
        "0",
        "960",
        "540",
        "0",
    ]);
    let layer = [200, 100, 50];
    let background = [10, 20, 30];
    let render = |phase: &str, frame: Option<&str>| {
        let name = format!("{phase}-{}.ppm", frame.unwrap_or("hold"));
        let output = root.join(name);
        let mut arguments = vec!["scene-render-frame", path, "10", "--phase", phase];
        if let Some(frame) = frame {
            arguments.extend(["--frame", frame]);
        }
        arguments.push(output.to_str().unwrap());
        let printed = run(&arguments);
        assert!(
            printed.starts_with(&format!(
                "scene-render-frame: scene 10 phase={phase} frame={} 640x360 to ",
                frame.unwrap_or("0")
            )),
            "{printed}"
        );
        output
    };
    // The layer's right edge, in output pixels, or `None` when hidden.
    let right_edge = |image: &Path| {
        (0..640)
            .rev()
            .find(|x| ppm_pixel(image, 640, 360, *x, 100) == layer)
            .map(|x| x + 1)
    };
    // Without animation every point of every phase is the static design.
    assert_eq!(right_edge(&render("in", Some("0"))), Some(320));
    assert_eq!(right_edge(&render("hold", None)), Some(320));

    assert_eq!(
        run(&[
            "scene-animation-preset",
            path,
            "10",
            "1",
            "--phase",
            "in",
            "--preset",
            "slide-from-left",
            "--frames",
            "10",
        ]),
        "scene-animation-preset: scene 10 revision=1 in_frames=10 out_frames=0 tracks=1"
    );
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let scene = stored
        .project()
        .scenes()
        .iter()
        .find(|scene| scene.id.get().get() == 10)
        .unwrap();
    assert_eq!(scene.animation.revision, 1);
    assert_eq!(scene.animation.clip.in_frames(), 10);

    // Frame 0 starts fully off the left edge (x = 0 - 960); frame 10 and
    // the hold are the static design; halfway CubicOut is 0.875 of the way:
    // -960 + 840 = -120, so the right edge is at 840 / 3 = 280.
    assert_eq!(right_edge(&render("in", Some("0"))), None);
    assert_eq!(right_edge(&render("in", Some("5"))), Some(280));
    assert_eq!(right_edge(&render("in", Some("10"))), Some(320));
    assert_eq!(right_edge(&render("hold", None)), Some(320));
    let halfway = render("in", Some("5"));
    assert_eq!(ppm_pixel(&halfway, 640, 360, 280, 100), background);
    assert_eq!(ppm_pixel(&halfway, 640, 360, 0, 179), layer);
    assert_eq!(ppm_pixel(&halfway, 640, 360, 0, 180), background);

    // Stretching the In phase to 20 frames scales its keys: frame 10 is
    // the old halfway pose.
    assert_eq!(
        run(&[
            "scene-animation-stretch",
            path,
            "10",
            "--phase",
            "in",
            "--frames",
            "20",
        ]),
        "scene-animation-stretch: scene 10 revision=2 in_frames=20 out_frames=0 tracks=1"
    );
    assert_eq!(
        fs::read(render("in", Some("10"))).unwrap(),
        fs::read(&halfway).unwrap()
    );

    // An Out fade ends fully transparent; its first frame is the design.
    run(&[
        "scene-animation-preset",
        path,
        "10",
        "1",
        "--phase",
        "out",
        "--preset",
        "fade",
        "--frames",
        "4",
    ]);
    assert_eq!(right_edge(&render("out", Some("0"))), Some(320));
    assert_eq!(right_edge(&render("out", Some("4"))), None);
    let fading = render("out", Some("2"));
    let blended = ppm_pixel(&fading, 640, 360, 0, 0);
    assert!(
        blended != layer && blended != background,
        "halfway through the fade blends: {blended:?}"
    );

    // Refusals leave the bundle untouched.
    let before = manifest(&project);
    assert_failure_contains(
        &invoke_bounded(&[
            "scene-animation-preset",
            path,
            "10",
            "9",
            "--phase",
            "in",
            "--preset",
            "fade",
            "--frames",
            "4",
        ]),
        "unknown scene layer 9",
    );
    assert_failure_contains(
        &invoke_bounded(&[
            "scene-animation-preset",
            path,
            "10",
            "1",
            "--phase",
            "in",
            "--preset",
            "spin",
            "--frames",
            "4",
        ]),
        "unknown animation preset `spin`",
    );
    assert_failure_contains(
        &invoke_bounded(&[
            "scene-animation-stretch",
            path,
            "99",
            "--phase",
            "in",
            "--frames",
            "4",
        ]),
        "scene 99 does not exist",
    );
    assert_eq!(manifest(&project), before);

    fs::remove_dir_all(root).unwrap();
}

/// A clip of `tracks` targets with 128 dense cubic-bezier keys each, a shape
/// whose wire encoding is near the 36 KiB animation bound.
fn dense_clip<T: Copy + Ord + core::fmt::Debug>(
    tracks: u32,
    target: impl Fn(u32) -> T,
) -> fm_model::fm_animation::Clip<T> {
    use fm_model::fm_animation::{
        Clip, CubicBezier, Easing, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value,
    };
    const KEYS: u32 = 128;
    let bezier = Easing::CubicBezier(CubicBezier::new(65_536, -65_536, 65_536, 131_072).unwrap());
    Clip::from_tracks(
        Timebase::new(fm_types::FrameRate::new(60, 1).unwrap()),
        KEYS - 1,
        0,
        (1..=tracks)
            .map(|index| {
                Track::new(
                    TrackKey::new(target(index), Phase::In, Property::PositionX),
                    (0..KEYS)
                        .map(|frame| {
                            Keyframe::new(frame, Value::Scalar(i64::from(i32::MIN)), bezier)
                        })
                        .collect(),
                )
            })
            .collect(),
    )
    .unwrap()
}

/// Local scene and title animation edits are held to the same wire bound as
/// daemon edits: a stretch that grows a fitting clip past
/// `MAX_*_ANIMATION_BYTES` is refused with a clear error and nothing is
/// saved.
#[test]
#[allow(clippy::too_many_lines)] // one refusal per authoring surface
fn local_animation_edits_refuse_clips_without_a_wire_encoding() {
    use fm_model::{LayerId, SceneAnimation, fm_animation::MAX_PHASE_FRAMES};
    use fm_protocol::{
        MAX_SCENE_ANIMATION_BYTES, MAX_TITLE_ANIMATION_BYTES, scene_clip_wire_bytes,
        title_clip_wire_bytes,
    };
    use fm_titles::{ElementId, TitleAnimation};

    let root = unique_test_root();
    let project = root.join("bound.freemix");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    let run = |arguments: &[&str]| {
        let output = invoke_bounded(arguments);
        assert_success(&output);
        stdout(&output)
    };

    // Scene: the most layers whose dense clip still fits.
    let layer = |index: u32| LayerId::new(index).unwrap();
    let tracks = (1..=32)
        .take_while(|tracks| {
            scene_clip_wire_bytes(&fm_control::scene_clip_to_wire(&dense_clip(*tracks, layer)))
                <= MAX_SCENE_ANIMATION_BYTES
        })
        .last()
        .unwrap();
    run(&["new", path]);
    run(&["scene-input-add", path, "3", "10", "Cam"]);
    for index in 0..tracks {
        run(&[
            "scene-layer-add",
            path,
            "10",
            "1",
            "0",
            &format!("L{index}"),
        ]);
    }
    let store = ProjectStore::new(&project).unwrap();
    let stored = store.load().unwrap();
    let mut edited = stored.project().clone();
    let scene = SceneId::new(NonZeroU128::new(10).unwrap());
    edited
        .set_scene_animation(scene, SceneAnimation::new(dense_clip(tracks, layer), 0))
        .unwrap();
    store
        .save(
            &StoredProject::from_project_with_complete_runtime_state(
                edited,
                stored.runtime_routing(),
                stored.runtime_manual_transitions(),
                stored.runtime_fade_to_black(),
                stored.runtime_overlays().clone(),
                stored.position(),
                stored.idempotency_receipts().to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    let manifest = fs::read(project.join("project.json")).unwrap();
    let frames = MAX_PHASE_FRAMES.to_string();
    assert_failure_contains(
        &invoke_bounded(&[
            "scene-animation-stretch",
            path,
            "10",
            "--phase",
            "in",
            "--frames",
            &frames,
        ]),
        &format!(
            "scene 10: the edited animation exceeds the {MAX_SCENE_ANIMATION_BYTES} byte \
             protocol limit (MAX_SCENE_ANIMATION_BYTES) and was not saved"
        ),
    );
    assert_eq!(fs::read(project.join("project.json")).unwrap(), manifest);
    // A stretch that keeps the clip on the wire still applies.
    assert!(
        run(&[
            "scene-animation-stretch",
            path,
            "10",
            "--phase",
            "in",
            "--frames",
            "127"
        ])
        .starts_with("scene-animation-stretch: scene 10 revision=1 ")
    );

    // Title: the same shape over template elements.
    let element = |index: u32| ElementId::new(NonZeroU128::new(u128::from(index)).unwrap());
    let tracks = (1..=32)
        .take_while(|tracks| {
            title_clip_wire_bytes(&fm_control::title_clip_to_wire(&dense_clip(
                *tracks, element,
            ))) <= MAX_TITLE_ANIMATION_BYTES
        })
        .last()
        .unwrap();
    let elements = (1..=tracks)
        .map(|id| {
            format!(
                r##"{{"id": {id}, "name": "Box {id}", "bounds": {{"x": 1, "y": 1, "width": 2, "height": 2}}, "z_index": 0, "visible": true, "style": {{"fill": "#ff0000ff", "background": null, "opacity": 255, "font": null, "alignment": {{"horizontal": "start", "vertical": "start"}}}}, "color_field": null, "kind": {{"type": "rectangle"}}}}"##
            )
        })
        .collect::<Vec<_>>()
        .join(",\n      ");
    let source = root.join("dense.fmtitle");
    fs::write(
        &source,
        SLIDE_TITLE_SOURCE.replace(
            r##"{"id": 1, "name": "Bar", "bounds": {"x": 4, "y": 2, "width": 8, "height": 4}, "z_index": 0, "visible": true, "style": {"fill": "#ff0000ff", "background": null, "opacity": 255, "font": null, "alignment": {"horizontal": "start", "vertical": "start"}}, "color_field": null, "kind": {"type": "rectangle"}}"##,
            &elements,
        ),
    )
    .unwrap();
    run(&[
        "title-input-add",
        path,
        "--id",
        "9",
        "--name",
        "Dense",
        "--template",
        source.to_str().unwrap(),
    ]);
    let uri = "asset://titles/dense.fmtitle";
    let mut template = store.load_title_template(uri).unwrap();
    template.template.animation = TitleAnimation::new(dense_clip(tracks, element), 0);
    store.replace_title_template(uri, &template).unwrap();
    let stored_template = fs::read(project.join("assets/titles/dense.fmtitle")).unwrap();
    assert_failure_contains(
        &invoke_bounded(&[
            "title-animation-stretch",
            path,
            uri,
            "--phase",
            "in",
            "--frames",
            &frames,
        ]),
        &format!(
            "title template {uri}: the edited animation exceeds the \
             {MAX_TITLE_ANIMATION_BYTES} byte protocol limit (MAX_TITLE_ANIMATION_BYTES) and \
             was not saved"
        ),
    );
    assert_eq!(
        fs::read(project.join("assets/titles/dense.fmtitle")).unwrap(),
        stored_template
    );
    fs::remove_dir_all(root).unwrap();
}

/// GX-011: a scene layer's rotation key turns it by any angle about its
/// centre (no quarter-turn snapping) in the locally rendered frame.
#[test]
fn scene_rotation_renders_at_any_angle() {
    use fm_model::{
        LayerId, SceneAnimation,
        fm_animation::{Clip, Easing, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value},
    };

    let root = unique_test_root();
    let project = root.join("tilt.freemix");
    fs::create_dir_all(&root).unwrap();
    let path = project.to_str().unwrap();
    let run = |arguments: &[&str]| {
        let output = invoke_bounded(arguments);
        assert_success(&output);
        stdout(&output)
    };
    run(&["new", path]);
    run(&["input-replace-solid", path, "1", "200", "100", "50", "255"]);
    run(&["scene-input-add", path, "3", "10", "Cam"]);
    run(&["scene-background", path, "10", "10", "20", "30", "255"]);
    run(&["scene-layer-add", path, "10", "1", "0", "L1"]);
    // 960x540 at (480, 270): centred on the 1920x1080 canvas.
    run(&[
        "scene-layer-geometry",
        path,
        "10",
        "0",
        "480",
        "270",
        "960",
        "540",
        "0",
    ]);
    // A constant 30 degree clockwise turn through the In phase.
    let clip = Clip::from_tracks(
        Timebase::new(fm_types::FrameRate::new(30, 1).unwrap()),
        10,
        0,
        vec![Track::new(
            TrackKey::new(LayerId::new(1).unwrap(), Phase::In, Property::Rotation),
            vec![
                Keyframe::new(0, Value::Scalar(30_000), Easing::Linear),
                Keyframe::new(10, Value::Scalar(30_000), Easing::Linear),
            ],
        )],
    )
    .unwrap();
    let store = ProjectStore::new(&project).unwrap();
    let stored = store.load().unwrap();
    let mut edited = stored.project().clone();
    edited
        .set_scene_animation(
            SceneId::new(NonZeroU128::new(10).unwrap()),
            SceneAnimation::new(clip, 1),
        )
        .unwrap();
    store
        .save(
            &StoredProject::from_project_with_complete_runtime_state(
                edited,
                stored.runtime_routing(),
                stored.runtime_manual_transitions(),
                stored.runtime_fade_to_black(),
                stored.runtime_overlays().clone(),
                stored.position(),
                stored.idempotency_receipts().to_vec(),
            )
            .unwrap(),
        )
        .unwrap();
    let render = |phase: &str, frame: &str| {
        let output = root.join(format!("{phase}.ppm"));
        run(&[
            "scene-render-frame",
            path,
            "10",
            "--phase",
            phase,
            "--frame",
            frame,
            output.to_str().unwrap(),
        ]);
        output
    };
    let layer = [200, 100, 50];
    let background = [10, 20, 30];
    // The Out phase has no tracks, so it draws the static design: the box
    // spans output (160..480, 90..270).
    let held = render("out", "0");
    assert_eq!(ppm_pixel(&held, 640, 360, 162, 92), layer);
    assert_eq!(ppm_pixel(&held, 640, 360, 320, 275), background);
    // Turned 30 degrees about the canvas centre (320, 180): the untilted
    // top-left corner is uncovered, and the box reaches below its untilted
    // bottom edge on the centre column (half height 270 / cos 30 = 311.8
    // project px, 103.9 output px) and right of centre below it.
    let tilted = render("in", "5");
    assert_eq!(ppm_pixel(&tilted, 640, 360, 162, 92), background);
    assert_eq!(ppm_pixel(&tilted, 640, 360, 320, 180), layer);
    assert_eq!(ppm_pixel(&tilted, 640, 360, 320, 275), layer);
    assert_eq!(ppm_pixel(&tilted, 640, 360, 320, 290), background);
    // Clockwise: the right end swings down, the left end up.
    assert_eq!(ppm_pixel(&tilted, 640, 360, 440, 260), layer);
    assert_eq!(ppm_pixel(&tilted, 640, 360, 200, 260), background);
    fs::remove_dir_all(root).unwrap();
}
