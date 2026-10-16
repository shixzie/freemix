#![cfg(feature = "std-tcp")]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroU128;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_client::{
    Client, ClientConfig, ClientError, CommandStatus, ConnectionState, DisconnectCause, Intake,
    SessionEvent, SyncMode, TcpSession, TcpSessionError,
};
use fm_protocol::{
    AudioMeterChannel, AudioMetersMessage, CURRENT_PROTOCOL_VERSION, CapabilityReportSummary,
    ClientType, CodecError, CommandPayload, CommandResult, DiagnosticsResponse, EngineIdentity,
    EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    HandshakeOutcome, HandshakeRequest, HandshakeResponse, InputAudioMeters, LineDecoder,
    MAX_LINE_BYTES, ManualTransitionStatus, MonitorFrameMessage, MonitorFrameOrigin, MonitorTap,
    OverlayStatus, ProtocolVersion, RecordingStatus, ResumeCursor, Role, RuntimeEventMessage,
    RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage, SnapshotReason, StreamRealizedState,
    StreamStatusMessage, StreamStatusSample, WireInputId, WireMessage, WireRecorderSlot,
    WireStreamTargetId, encode_line, encode_qoi_rgb, monitor_frame_parts,
};
use fm_types::ProjectId;
use fm_ui_model::ModelError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn input_statuses() -> Vec<fm_protocol::InputStatus> {
    [(1, "Camera"), (2, "Slides")]
        .into_iter()
        .map(|(value, name)| fm_protocol::InputStatus {
            input: input(value),
            name: name.into(),
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
        })
        .collect()
}

fn recordings() -> Vec<RecordingStatus> {
    [1, 2]
        .into_iter()
        .map(|slot| RecordingStatus {
            recorder: WireRecorderSlot::new(slot).unwrap(),
            desired_active: false,
        })
        .collect()
}

fn project_id() -> ProjectId {
    ProjectId::new(NonZeroU128::new(7).unwrap())
}

fn engine() -> EngineIdentity {
    EngineIdentity {
        engine_id: "engine-a".to_owned(),
        state_epoch: 3,
        log_id: "log-a".to_owned(),
    }
}

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".to_owned(),
        project_id: project_id().to_string(),
        state_epoch: 3,
        log_id: "log-a".to_owned(),
    }
}

fn client(capacity: usize) -> Client {
    client_with_completed_history(capacity, fm_client::DEFAULT_COMPLETED_COMMAND_CAPACITY)
}

fn client_with_completed_history(capacity: usize, completed_command_capacity: usize) -> Client {
    let mut config = ClientConfig::new(
        "tcp-test",
        ClientType::Integration,
        Role::Operator,
        "tcp-client",
        project_id(),
    );
    config.outbound_capacity = capacity;
    config.completed_command_capacity = completed_command_capacity;
    config.initial_backoff_ms = 10;
    config.max_backoff_ms = 40;
    Client::new(config).unwrap()
}

fn handshake(revision: u64, outcome: HandshakeOutcome) -> HandshakeResponse {
    handshake_version(CURRENT_PROTOCOL_VERSION, revision, outcome)
}

fn handshake_version(
    protocol: ProtocolVersion,
    revision: u64,
    outcome: HandshakeOutcome,
) -> HandshakeResponse {
    HandshakeResponse {
        protocol,
        granted_role: Role::Operator,
        permissions: vec!["switcher.take".to_owned()],
        capabilities: CapabilityReportSummary {
            digest: "sha256:test".to_owned(),
            total: 1,
            available: 1,
            degraded: 0,
            unavailable: 0,
        },
        server: server(),
        current_revision: revision,
        outcome,
    }
}

fn snapshot(revision: u64) -> SnapshotMessage {
    SnapshotMessage {
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        capture_inputs: Vec::new(),
        engine: engine(),
        revision,
        show_name: "Show".to_owned(),
        inputs: input_statuses(),
        outputs: Vec::new(),
        input_audio_strips: input_audio_strips(),
        desired_program: input(1),
        desired_preview: input(2),
        realized_program: input(1),
        realized_preview: input(2),
        desired_manual_transition: ManualTransitionStatus::Inactive,
        realized_manual_transition: ManualTransitionStatus::Inactive,
        desired_fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        realized_fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: recordings(),
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
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
    }
}

fn event(revision: u64) -> EventMessage {
    EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: EventCursor {
            engine: engine(),
            revision,
        },
        payload: EventPayload::DesiredSwitcher {
            program: input(2),
            preview: input(1),
            manual_transition: ManualTransitionStatus::Inactive,
            fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            overlays: OverlayStatus::empty_channels(),
            input_audio_strips: input_audio_strips(),
        },
    }
}

fn runtime_event(revision: u64) -> RuntimeEventMessage {
    RuntimeEventMessage {
        server: server(),
        revision,
        generation: 1,
        sequence: 1,
        event: RuntimeLifecycleEvent::Realized {
            domain: "switcher".to_owned(),
            manual_transition: ManualTransitionStatus::Inactive,
            fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
        },
    }
}

fn audio_meters(sequence: u64) -> AudioMetersMessage {
    AudioMetersMessage {
        server: server(),
        sequence,
        frame: sequence,
        start_sample: sequence * 1_920,
        end_sample: (sequence + 1) * 1_920,
        master: vec![AudioMeterChannel {
            peak_millionths: 500_000,
            rms_millionths: 250_000,
        }],
        inputs: vec![InputAudioMeters {
            input: input(1),
            channels: vec![AudioMeterChannel {
                peak_millionths: 250_000,
                rms_millionths: 125_000,
            }],
        }],
        monitor: None,
    }
}

fn stream_target() -> WireStreamTargetId {
    WireStreamTargetId::new(NonZeroU128::new(40).unwrap())
}

fn stream_status(sequence: u64) -> StreamStatusMessage {
    StreamStatusMessage {
        server: server(),
        sequence,
        samples: vec![StreamStatusSample {
            target: stream_target(),
            realized: StreamRealizedState::Live,
            connected: true,
            muxed_bytes: sequence * 512,
            enqueued_pairs: sequence * 30,
            dropped_pairs: 0,
            failure: None,
            local: None,
        }],
    }
}

fn accept_snapshot(peer: &mut Peer, revision: u64) -> HandshakeRequest {
    accept_snapshot_version(peer, CURRENT_PROTOCOL_VERSION, revision)
}

fn accept_snapshot_version(
    peer: &mut Peer,
    protocol: ProtocolVersion,
    revision: u64,
) -> HandshakeRequest {
    let WireMessage::HandshakeRequest(request) = peer.receive() else {
        panic!("adapter emitted a non-current or invalid handshake")
    };
    peer.send(&WireMessage::HandshakeResponse(handshake_version(
        protocol,
        revision,
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::NoCursor,
        },
    )));
    peer.send(&WireMessage::Snapshot(snapshot(revision)));
    request
}

fn accept_resume(peer: &mut Peer, revision: u64) -> ResumeCursor {
    accept_resume_version(peer, CURRENT_PROTOCOL_VERSION, revision)
}

fn accept_resume_version(
    peer: &mut Peer,
    protocol: ProtocolVersion,
    revision: u64,
) -> ResumeCursor {
    let WireMessage::HandshakeRequest(request) = peer.receive() else {
        panic!("adapter emitted a non-current or invalid handshake")
    };
    let cursor = request.resume_cursor.expect("resume cursor");
    peer.send(&WireMessage::HandshakeResponse(handshake_version(
        protocol,
        revision,
        HandshakeOutcome::Resume {
            cursor: cursor.clone(),
        },
    )));
    cursor
}

fn spawn_server(run: impl FnOnce(TcpListener) + Send + 'static) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || run(listener));
    (address, handle)
}

struct Peer {
    stream: TcpStream,
    decoder: LineDecoder,
    pending: VecDeque<WireMessage>,
}

impl Peer {
    fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = listener.accept().unwrap();
        Self {
            stream,
            decoder: LineDecoder::new(),
            pending: VecDeque::new(),
        }
    }

    fn receive(&mut self) -> WireMessage {
        loop {
            if let Some(message) = self.pending.pop_front() {
                return message;
            }
            let mut buffer = [0_u8; 4096];
            let read = self.stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "unexpected client EOF");
            self.pending
                .extend(self.decoder.push(&buffer[..read]).unwrap());
        }
    }

    /// Writes every record in one burst so the client observes them
    /// regardless of when it reacts to an earlier one.
    fn send_all(&mut self, messages: &[WireMessage]) {
        let bytes: String = messages
            .iter()
            .map(|message| encode_line(message).unwrap())
            .collect();
        self.stream.write_all(bytes.as_bytes()).unwrap();
        self.stream.flush().unwrap();
    }

    fn send(&mut self, message: &WireMessage) {
        self.stream
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.stream.flush().unwrap();
    }
}

#[test]
fn snapshot_heartbeat_command_result_and_events_preserve_wire_order() {
    let (address, server_thread) = spawn_server(|listener| {
        let mut peer = Peer::accept(&listener);
        let request = accept_snapshot(&mut peer, 4);
        assert_eq!(request.resume_cursor, None);

        let WireMessage::Heartbeat(heartbeat) = peer.receive() else {
            panic!("expected heartbeat before command")
        };
        assert_eq!(heartbeat.last_applied.unwrap().revision, 4);
        let WireMessage::Command(command) = peer.receive() else {
            panic!("expected command")
        };
        peer.send(&WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 5,
            scheduled_frame: Some(9),
        }));
        peer.send(&WireMessage::Event(event(5)));
        peer.send(&WireMessage::RuntimeEvent(runtime_event(5)));
    });

    let mut session = TcpSession::new(client(4));
    assert_eq!(
        session.connect(address, CONNECT_TIMEOUT).unwrap(),
        SessionEvent::Connected {
            mode: SyncMode::Snapshot
        }
    );
    session.queue_heartbeat(1_234).unwrap();
    let command = session
        .queue_command(CommandPayload::Cut, "cut-once", Some(4), None)
        .unwrap();
    assert_eq!(session.flush().unwrap(), 2);

    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::CommandResult {
            result: CommandResult::Accepted { ref id, .. },
            intake: Intake::ResultReconciled,
        } if id == &command.id
    ));
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::Event {
            intake: Intake::EventApplied,
            ..
        }
    ));
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::RuntimeEvent {
            intake: Intake::RuntimeEventObserved,
            ..
        }
    ));
    assert_eq!(session.client().last_applied_cursor().unwrap().revision, 5);
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::Disconnected {
            cause: DisconnectCause::Eof,
            backoff
        } if backoff.attempt == 1 && backoff.delay_ms == 10
    ));
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::NotConnected)
    ));
    server_thread.join().unwrap();
}

#[test]
fn audio_meters_are_ephemeral_and_strictly_ordered() {
    let (release_tx, release_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        release_rx.recv().unwrap();
        peer.send(&WireMessage::AudioMeters(audio_meters(5)));
        peer.send(&WireMessage::AudioMeters(audio_meters(5)));
    });

    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    assert!(
        session
            .receive_timeout(Duration::from_millis(10))
            .unwrap()
            .is_none()
    );
    release_tx.send(()).unwrap();
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::AudioMeters { meters } if meters.sequence == 5
    ));
    assert_eq!(session.client().last_applied_cursor().unwrap().revision, 4);
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::UnexpectedMessage)
    ));
    assert!(matches!(
        session.client().state(),
        ConnectionState::Backoff(_)
    ));
    assert!(session.latest_audio_meters().is_none());
    server_thread.join().unwrap();
}

#[test]
fn stream_status_is_ephemeral_and_strictly_ordered() {
    let (release_tx, release_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        release_rx.recv().unwrap();
        peer.send(&WireMessage::StreamStatus(stream_status(5)));
        peer.send(&WireMessage::StreamStatus(stream_status(6)));
        peer.send(&WireMessage::StreamStatus(stream_status(4)));
    });

    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    assert!(
        session
            .receive_timeout(Duration::from_millis(10))
            .unwrap()
            .is_none()
    );
    release_tx.send(()).unwrap();
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::StreamStatus { status } if status.sequence == 5
    ));
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::StreamStatus { status } if status.sequence == 6
    ));
    let latest = session.latest_stream_status().expect("retained status");
    assert_eq!(latest.sequence, 6);
    assert_eq!(latest.samples.len(), 1);
    assert_eq!(latest.samples[0].target, stream_target());
    assert_eq!(latest.samples[0].realized, StreamRealizedState::Live);
    assert_eq!(session.client().last_applied_cursor().unwrap().revision, 4);
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::UnexpectedMessage)
    ));
    assert!(matches!(
        session.client().state(),
        ConnectionState::Backoff(_)
    ));
    assert!(session.latest_stream_status().is_none());
    server_thread.join().unwrap();
}

#[test]
fn stream_status_rejects_foreign_server_identity() {
    let (release_tx, release_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        release_rx.recv().unwrap();
        let mut foreign = stream_status(5);
        foreign.server.engine_id = "engine-b".to_owned();
        peer.send(&WireMessage::StreamStatus(foreign));
    });

    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    release_tx.send(()).unwrap();
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::UnexpectedMessage)
    ));
    assert!(matches!(
        session.client().state(),
        ConnectionState::Backoff(_)
    ));
    assert!(session.latest_stream_status().is_none());
    server_thread.join().unwrap();
}

#[test]
fn reconnect_consumes_resume_events_before_ready() {
    let (release_tx, release_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut first = Peer::accept(&listener);
        accept_snapshot(&mut first, 4);
        release_rx.recv().unwrap();
        drop(first);

        let mut second = Peer::accept(&listener);
        let cursor = accept_resume(&mut second, 5);
        assert_eq!(cursor.revision, 4);
        second.send(&WireMessage::Event(event(5)));
    });

    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    release_tx.send(()).unwrap();
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::Disconnected { .. }
    ));
    assert_eq!(
        session.connect(address, CONNECT_TIMEOUT).unwrap(),
        SessionEvent::Connected {
            mode: SyncMode::Resume
        }
    );
    assert_eq!(session.client().state(), &ConnectionState::Ready);
    assert_eq!(session.client().last_applied_cursor().unwrap().revision, 5);
    server_thread.join().unwrap();
}

#[test]
fn unresolved_command_retries_original_envelope_but_completed_result_does_not() {
    let (first_command_tx, first_command_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut first = Peer::accept(&listener);
        accept_snapshot(&mut first, 4);
        let WireMessage::Command(command) = first.receive() else {
            panic!("expected first command")
        };
        first_command_tx.send(command.clone()).unwrap();
        drop(first);

        let mut second = Peer::accept(&listener);
        accept_resume(&mut second, 4);
        let WireMessage::Command(retried) = second.receive() else {
            panic!("expected unresolved retry")
        };
        assert_eq!(retried, command);
        second.send(&WireMessage::CommandResult(CommandResult::Accepted {
            id: retried.id,
            revision: 4,
            scheduled_frame: None,
        }));
        assert_eq!(second.stream.read(&mut [0_u8; 1]).unwrap(), 0);

        let mut third = Peer::accept(&listener);
        accept_resume(&mut third, 4);
        assert!(matches!(third.receive(), WireMessage::Heartbeat(_)));
    });

    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let command = session
        .queue_command(CommandPayload::Cut, "durable-key", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    assert_eq!(first_command_rx.recv().unwrap(), command);
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::Disconnected { .. }
    ));

    session.connect(address, CONNECT_TIMEOUT).unwrap();
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::CommandResult {
            intake: Intake::ResultReconciled,
            ..
        }
    ));
    assert!(matches!(
        session.client().command(&command.id).unwrap().status,
        CommandStatus::Completed(_)
    ));
    session.disconnect().unwrap();

    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session.queue_heartbeat(9_999).unwrap();
    assert_eq!(session.flush().unwrap(), 1);
    server_thread.join().unwrap();
}

#[test]
fn replayed_evicted_receipt_forces_snapshot_without_retransmitting_collision() {
    let (address, server_thread) = spawn_server(move |listener| {
        let mut first = Peer::accept(&listener);
        accept_snapshot(&mut first, 4);

        let WireMessage::Command(old) = first.receive() else {
            panic!("expected original command")
        };
        first.send(&WireMessage::CommandResult(CommandResult::Accepted {
            id: old.id.clone(),
            revision: 4,
            scheduled_frame: None,
        }));
        let WireMessage::Command(evictor) = first.receive() else {
            panic!("expected history evictor")
        };
        first.send(&WireMessage::CommandResult(CommandResult::Accepted {
            id: evictor.id,
            revision: 4,
            scheduled_frame: None,
        }));
        let WireMessage::Command(collision) = first.receive() else {
            panic!("expected key-collision command")
        };
        assert_eq!(collision.idempotency_key, old.idempotency_key);
        first.send(&WireMessage::CommandResult(CommandResult::Accepted {
            id: old.id,
            revision: 4,
            scheduled_frame: None,
        }));
        assert_eq!(first.stream.read(&mut [0_u8; 1]).unwrap(), 0);

        let mut second = Peer::accept(&listener);
        let request = accept_snapshot(&mut second, 4);
        assert_eq!(request.resume_cursor, None);
        assert!(matches!(second.receive(), WireMessage::Heartbeat(_)));
    });

    let mut session = TcpSession::new(client_with_completed_history(2, 1));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let old = session
        .queue_command(CommandPayload::Cut, "retained-server-key", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    session.receive().unwrap();
    session
        .queue_command(CommandPayload::Cut, "evictor", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    session.receive().unwrap();
    assert!(session.client().command(&old.id).is_none());

    let collision = session
        .queue_command(
            CommandPayload::SelectPreview { input: input(1) },
            "retained-server-key",
            Some(4),
            None,
        )
        .unwrap();
    session.flush().unwrap();
    let error = session.receive().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("authoritative snapshot is required")
    );
    assert!(matches!(
        error,
        TcpSessionError::ResyncRequired(inner)
            if matches!(inner.as_ref(), ClientError::IdempotencyReplayCollision {
                received_command_id,
                affected_command_ids,
            } if received_command_id == &old.id
                && affected_command_ids == &vec![collision.id.clone()])
    ));
    assert!(matches!(
        session.client().command(&collision.id).unwrap().status,
        CommandStatus::TerminalUncertain(_)
    ));
    assert_eq!(session.client().model().pending_commands(), []);
    assert_eq!(session.in_flight_len(), 1);

    assert!(matches!(
        session.connect(address, CONNECT_TIMEOUT).unwrap(),
        SessionEvent::Connected {
            mode: SyncMode::Snapshot
        }
    ));
    assert_eq!(session.in_flight_len(), 0);
    session.queue_heartbeat(9_999).unwrap();
    assert_eq!(session.flush().unwrap(), 1);
    server_thread.join().unwrap();
}

#[test]
fn in_flight_and_outbound_queues_remain_bounded() {
    let (command_tx, command_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        let command = peer.receive();
        command_tx.send(command).unwrap();
        release_rx.recv().unwrap();
    });

    let mut session = TcpSession::new(client(1));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session
        .queue_command(CommandPayload::Cut, "first", Some(4), None)
        .unwrap();
    assert_eq!(session.flush().unwrap(), 1);
    assert!(matches!(
        command_rx.recv().unwrap(),
        WireMessage::Command(_)
    ));
    session
        .queue_command(CommandPayload::Cut, "second", Some(4), None)
        .unwrap();
    assert_eq!(session.flush().unwrap(), 0);
    assert_eq!(session.in_flight_len(), 1);
    assert_eq!(session.client().outbound_len(), 1);
    assert!(matches!(
        session.queue_heartbeat(0),
        Err(TcpSessionError::Client(ClientError::QueueFull {
            capacity: 1
        }))
    ));
    release_tx.send(()).unwrap();
    server_thread.join().unwrap();
}

#[test]
fn oversized_line_disconnects_once_and_enters_backoff() {
    let (address, server_thread) = spawn_server(|listener| {
        let mut peer = Peer::accept(&listener);
        assert!(matches!(peer.receive(), WireMessage::HandshakeRequest(_)));
        peer.stream
            .write_all(&vec![b'x'; MAX_LINE_BYTES + 1])
            .unwrap();
        peer.stream.flush().unwrap();
    });

    let mut session = TcpSession::new(client(2));
    assert!(matches!(
        session.connect(address, CONNECT_TIMEOUT),
        Err(TcpSessionError::Codec(CodecError::LineTooLong))
    ));
    assert_eq!(session.reconnect_backoff().unwrap().attempt, 1);
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::NotConnected)
    ));
    assert_eq!(session.reconnect_backoff().unwrap().attempt, 1);
    server_thread.join().unwrap();
}

#[test]
fn wrong_first_record_and_wrong_project_reject_current_handshake() {
    let (address, wrong_record_thread) = spawn_server(|listener| {
        let mut peer = Peer::accept(&listener);
        assert!(matches!(peer.receive(), WireMessage::HandshakeRequest(_)));
        peer.send(&WireMessage::Snapshot(snapshot(0)));
    });
    let mut wrong_record = TcpSession::new(client(2));
    assert!(matches!(
        wrong_record.connect(address, CONNECT_TIMEOUT),
        Err(TcpSessionError::ExpectedHandshake)
    ));
    assert_eq!(wrong_record.reconnect_backoff().unwrap().attempt, 1);
    wrong_record_thread.join().unwrap();

    let (address, wrong_project_thread) = spawn_server(|listener| {
        let mut peer = Peer::accept(&listener);
        assert!(matches!(peer.receive(), WireMessage::HandshakeRequest(_)));
        let mut response = handshake(
            0,
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        );
        response.server.project_id = "999".to_owned();
        peer.send(&WireMessage::HandshakeResponse(response));
    });
    let mut wrong_project = TcpSession::new(client(2));
    assert!(matches!(
        wrong_project.connect(address, CONNECT_TIMEOUT),
        Err(TcpSessionError::Client(ClientError::InvalidHandshake(
            "server selected a different project"
        )))
    ));
    assert_eq!(wrong_project.reconnect_backoff().unwrap().attempt, 1);
    wrong_project_thread.join().unwrap();
}

#[test]
fn protocol_mismatch_remains_terminal_after_socket_close() {
    let (address, server_thread) = spawn_server(|listener| {
        let mut peer = Peer::accept(&listener);
        assert!(matches!(peer.receive(), WireMessage::HandshakeRequest(_)));
        let mut response = handshake(
            0,
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        );
        response.protocol = ProtocolVersion::new(99, 0);
        peer.send(&WireMessage::HandshakeResponse(response));
        assert_eq!(peer.stream.read(&mut [0_u8; 1]).unwrap(), 0);
    });
    let mut session = TcpSession::new(client(2));

    assert!(matches!(
        session.connect(address, CONNECT_TIMEOUT),
        Err(TcpSessionError::Client(ClientError::ProtocolMismatch(
            ProtocolVersion {
                major: 99,
                minor: 0
            }
        )))
    ));
    assert_eq!(
        session.client().state(),
        &ConnectionState::ProtocolMismatch {
            protocol: ProtocolVersion::new(99, 0)
        }
    );
    assert_eq!(session.reconnect_backoff(), None);
    assert!(session.connection().is_none());
    server_thread.join().unwrap();
}

#[test]
fn encoding_failure_disconnects_and_preserves_unresolved_envelope() {
    let (address, server_thread) = spawn_server(|listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        assert_eq!(peer.stream.read(&mut [0_u8; 1]).unwrap(), 0);
    });
    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let command = session
        .queue_command(
            CommandPayload::Cut,
            "x".repeat(MAX_LINE_BYTES),
            Some(4),
            None,
        )
        .unwrap();

    assert!(matches!(
        session.flush(),
        Err(TcpSessionError::Codec(
            CodecError::LineTooLong | CodecError::FieldValueTooLong
        ))
    ));
    assert_eq!(session.in_flight_len(), 1);
    assert_eq!(session.reconnect_backoff().unwrap().attempt, 1);
    assert!(session.connection().is_none());
    let record = session.client().command(&command.id).unwrap();
    assert_eq!(record.command, command);
    assert_eq!(record.status, CommandStatus::Sent);
    server_thread.join().unwrap();
}

#[test]
fn silent_handshake_can_be_cancelled_without_waiting_for_peer_eof() {
    let (request_tx, request_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        assert!(matches!(peer.receive(), WireMessage::HandshakeRequest(_)));
        request_tx.send(()).unwrap();
        assert_eq!(peer.stream.read(&mut [0_u8; 1]).unwrap(), 0);
    });

    let mut session = TcpSession::new(client(2));
    let mut polls = 0;
    let started = Instant::now();
    let result =
        session.connect_cancellable(address, CONNECT_TIMEOUT, Duration::from_millis(10), || {
            polls += 1;
            polls >= 7
        });
    request_rx.recv().unwrap();
    assert!(matches!(
        result,
        Err(TcpSessionError::Cancelled { backoff })
            if backoff.attempt == 1 && backoff.delay_ms == 10
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    server_thread.join().unwrap();
}

#[test]
fn silent_command_response_can_be_cancelled_and_retried() {
    let (command_tx, command_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        assert!(matches!(peer.receive(), WireMessage::Command(_)));
        command_tx.send(()).unwrap();
        assert_eq!(peer.stream.read(&mut [0_u8; 1]).unwrap(), 0);
    });

    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session
        .queue_command(CommandPayload::Cut, "silent-command", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    command_rx.recv().unwrap();

    let mut polls = 0;
    assert!(matches!(
        session.receive_cancellable(Duration::from_millis(10), || {
            polls += 1;
            polls >= 3
        }),
        Err(TcpSessionError::Cancelled { backoff })
            if backoff.attempt == 1 && backoff.delay_ms == 10
    ));
    assert_eq!(session.in_flight_len(), 1);
    assert_eq!(session.reconnect_backoff().unwrap().attempt, 1);
    server_thread.join().unwrap();
}

#[test]
fn cancellable_receive_preserves_a_partial_framed_record() {
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        let WireMessage::Command(command) = peer.receive() else {
            panic!("expected command")
        };
        let encoded = encode_line(&WireMessage::CommandResult(CommandResult::Accepted {
            id: command.id,
            revision: 4,
            scheduled_frame: None,
        }))
        .unwrap();
        let split = encoded.len() / 2;
        peer.stream.write_all(&encoded.as_bytes()[..split]).unwrap();
        peer.stream.flush().unwrap();
        thread::sleep(Duration::from_millis(40));
        peer.stream.write_all(&encoded.as_bytes()[split..]).unwrap();
        peer.stream.flush().unwrap();
    });

    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let command = session
        .queue_command(CommandPayload::Cut, "partial-result", Some(4), None)
        .unwrap();
    session.flush().unwrap();

    let mut polls = 0;
    assert!(matches!(
        session
            .receive_cancellable(Duration::from_millis(5), || {
                polls += 1;
                false
            })
            .unwrap(),
        SessionEvent::CommandResult {
            result: CommandResult::Accepted { ref id, .. },
            ..
        } if id == &command.id
    ));
    assert!(polls > 1, "partial record did not span polling cycles");
    server_thread.join().unwrap();
}

#[test]
fn tcp_establishment_checks_cancellation_before_blocking() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut session = TcpSession::new(client(2));
    let started = Instant::now();

    assert!(matches!(
        session.connect_cancellable(
            address,
            CONNECT_TIMEOUT,
            Duration::from_millis(10),
            || true,
        ),
        Err(TcpSessionError::Cancelled { backoff }) if backoff.attempt == 1
    ));
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(listener.set_nonblocking(true).is_ok());
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn event_gap_forces_snapshot_and_preserves_unresolved_command() {
    let (original_tx, original_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut first = Peer::accept(&listener);
        accept_snapshot(&mut first, 4);
        let WireMessage::Command(original) = first.receive() else {
            panic!("expected original command")
        };
        original_tx.send(original.clone()).unwrap();
        first.send(&WireMessage::Event(event(6)));
        assert_eq!(first.stream.read(&mut [0_u8; 1]).unwrap(), 0);

        let mut second = Peer::accept(&listener);
        let WireMessage::HandshakeRequest(request) = second.receive() else {
            panic!("expected snapshot reconnect")
        };
        assert_eq!(request.resume_cursor, None);
        second.send(&WireMessage::HandshakeResponse(handshake(
            6,
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::HistoryUnavailable,
            },
        )));
        second.send(&WireMessage::Snapshot(snapshot(6)));
        let WireMessage::Command(retried) = second.receive() else {
            panic!("expected unresolved command retry")
        };
        assert_eq!(retried, original);
    });

    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let command = session
        .queue_command(CommandPayload::Cut, "event-gap", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    assert_eq!(original_rx.recv().unwrap(), command);
    assert!(
        matches!(session.receive(), Err(TcpSessionError::ResyncRequired(error))
        if matches!(error.as_ref(), ClientError::ResyncRequired {
                expected_revision: 5,
                received_revision: 6,
            }))
    );
    assert_eq!(session.in_flight_len(), 1);
    assert_eq!(session.reconnect_backoff().unwrap().attempt, 1);
    assert!(matches!(
        session.connect(address, CONNECT_TIMEOUT).unwrap(),
        SessionEvent::Connected {
            mode: SyncMode::Snapshot
        }
    ));
    server_thread.join().unwrap();
}

#[test]
fn runtime_sequence_gap_enters_backoff_and_retries_unresolved_command() {
    let (original_tx, original_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut first = Peer::accept(&listener);
        first
            .stream
            .set_read_timeout(Some(CONNECT_TIMEOUT))
            .unwrap();
        accept_snapshot(&mut first, 4);
        let WireMessage::Command(original) = first.receive() else {
            panic!("expected original command")
        };
        original_tx.send(original.clone()).unwrap();
        first.send(&WireMessage::RuntimeEvent(runtime_event(4)));
        let mut gap = runtime_event(4);
        gap.sequence = 3;
        first.send(&WireMessage::RuntimeEvent(gap));
        assert_eq!(first.stream.read(&mut [0_u8; 1]).unwrap(), 0);

        let mut second = Peer::accept(&listener);
        second
            .stream
            .set_read_timeout(Some(CONNECT_TIMEOUT))
            .unwrap();
        assert_eq!(accept_resume(&mut second, 4).revision, 4);
        let WireMessage::Command(retried) = second.receive() else {
            panic!("expected unresolved command retry")
        };
        assert_eq!(retried, original);
    });

    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let command = session
        .queue_command(CommandPayload::Cut, "runtime-gap", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    assert_eq!(original_rx.recv().unwrap(), command);
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::RuntimeEvent {
            intake: Intake::RuntimeEventObserved,
            ..
        }
    ));
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::ResyncRequired(error))
            if matches!(error.as_ref(), ClientError::RuntimeSequenceGap {
                generation: 1,
                expected_sequence: 2,
                received_sequence: 3,
            })
    ));
    assert_eq!(session.in_flight_len(), 1);
    assert_eq!(session.reconnect_backoff().unwrap().attempt, 1);
    assert!(matches!(
        session.connect(address, CONNECT_TIMEOUT).unwrap(),
        SessionEvent::Connected {
            mode: SyncMode::Resume
        }
    ));
    server_thread.join().unwrap();
}

#[test]
fn model_error_forces_snapshot_and_preserves_unresolved_command() {
    let (address, server_thread) = spawn_server(move |listener| {
        let mut first = Peer::accept(&listener);
        accept_snapshot(&mut first, 4);
        let WireMessage::Command(original) = first.receive() else {
            panic!("expected original command")
        };
        first.send(&WireMessage::Event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(99),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                overlays: OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        }));
        assert_eq!(first.stream.read(&mut [0_u8; 1]).unwrap(), 0);

        let mut second = Peer::accept(&listener);
        let WireMessage::HandshakeRequest(request) = second.receive() else {
            panic!("expected snapshot reconnect")
        };
        assert_eq!(request.resume_cursor, None);
        second.send(&WireMessage::HandshakeResponse(handshake(
            5,
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::HistoryUnavailable,
            },
        )));
        second.send(&WireMessage::Snapshot(snapshot(5)));
        let WireMessage::Command(retried) = second.receive() else {
            panic!("expected unresolved command retry")
        };
        assert_eq!(retried, original);
    });

    let mut session = TcpSession::new(client(2));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session
        .queue_command(CommandPayload::Cut, "model-error", Some(4), None)
        .unwrap();
    session.flush().unwrap();
    assert!(
        matches!(session.receive(), Err(TcpSessionError::ResyncRequired(error))
        if matches!(error.as_ref(), ClientError::Model(ModelError::UnknownInput(value))
            if *value == input(99).to_domain()))
    );
    assert_eq!(session.in_flight_len(), 1);
    assert!(matches!(
        session.connect(address, CONNECT_TIMEOUT).unwrap(),
        SessionEvent::Connected {
            mode: SyncMode::Snapshot
        }
    ));
    server_thread.join().unwrap();
}

fn input_audio_strips() -> Vec<fm_protocol::InputAudioStripStatus> {
    [input(1), input(2)]
        .into_iter()
        .map(|input| fm_protocol::InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
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

/// A deterministic, poorly compressible RGB image.
fn monitor_pixels(width: u16, height: u16, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    (0..usize::from(width) * usize::from(height) * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn monitor_frame(tap: MonitorTap, sequence: u64, width: u16, height: u16) -> Vec<WireMessage> {
    let pixels = monitor_pixels(width, height, u32::try_from(sequence).unwrap());
    let qoi = encode_qoi_rgb(width, height, &pixels, usize::from(width) * 3, 3).unwrap();
    monitor_frame_parts(
        &server(),
        tap,
        sequence,
        sequence * 10,
        input(1),
        MonitorFrameOrigin::Simulated,
        width,
        height,
        &qoi,
    )
    .into_iter()
    .map(WireMessage::MonitorFrame)
    .collect()
}

fn monitor_part(message: &WireMessage) -> MonitorFrameMessage {
    let WireMessage::MonitorFrame(part) = message else {
        panic!("expected a monitor_frame part");
    };
    part.clone()
}

fn connect_with_records(records: Vec<WireMessage>) -> (TcpSession, JoinHandle<()>) {
    let (release_tx, release_rx) = mpsc::channel();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        release_rx.recv().unwrap();
        peer.send_all(&records);
    });
    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    release_tx.send(()).unwrap();
    (session, server_thread)
}

fn assert_monitor_violation(session: &mut TcpSession) {
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::UnexpectedMessage)
    ));
    assert!(matches!(
        session.client().state(),
        ConnectionState::Backoff(_)
    ));
    for tap in MonitorTap::ALL {
        assert!(session.latest_monitor_frame(tap).is_none());
    }
}

#[test]
fn monitor_frames_assemble_per_tap_and_clear_on_disconnect() {
    let program = monitor_frame(MonitorTap::Program, 5, 200, 100);
    assert!(program.len() > 1, "the test frame must span several parts");
    let preview = monitor_frame(MonitorTap::Preview, 2, 4, 3);
    let newer_program = monitor_frame(MonitorTap::Program, 6, 2, 2);
    let mut records = program.clone();
    records.extend(preview);
    records.extend(newer_program);
    records.push(WireMessage::AudioMeters(audio_meters(1)));
    let (mut session, server_thread) = connect_with_records(records);

    for index in 0..program.len() {
        let complete = index + 1 == program.len();
        assert_eq!(
            session.receive().unwrap(),
            SessionEvent::MonitorFrame {
                tap: MonitorTap::Program,
                complete
            }
        );
        if !complete {
            assert!(session.latest_monitor_frame(MonitorTap::Program).is_none());
        }
    }
    let (image, received_at) = session.latest_monitor_frame(MonitorTap::Program).unwrap();
    assert!(received_at <= Instant::now());
    assert_eq!(
        (image.tap, image.sequence, image.frame, image.input),
        (MonitorTap::Program, 5, 50, input(1))
    );
    assert_eq!(image.origin, MonitorFrameOrigin::Simulated);
    assert_eq!((image.width, image.height), (200, 100));
    let expected: Vec<u8> = monitor_pixels(200, 100, 5)
        .chunks(3)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
        .collect();
    assert_eq!(&*image.rgba, expected.as_slice());
    assert!(session.latest_monitor_frame(MonitorTap::Preview).is_none());

    assert_eq!(
        session.receive().unwrap(),
        SessionEvent::MonitorFrame {
            tap: MonitorTap::Preview,
            complete: true
        }
    );
    let (preview, _) = session.latest_monitor_frame(MonitorTap::Preview).unwrap();
    assert_eq!((preview.sequence, preview.width, preview.height), (2, 4, 3));
    assert_eq!(preview.rgba.len(), 4 * 3 * 4);

    assert_eq!(
        session.receive().unwrap(),
        SessionEvent::MonitorFrame {
            tap: MonitorTap::Program,
            complete: true
        }
    );
    let (newer, _) = session.latest_monitor_frame(MonitorTap::Program).unwrap();
    assert_eq!((newer.sequence, newer.width), (6, 2));
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::AudioMeters { .. }
    ));
    assert_eq!(session.client().last_applied_cursor().unwrap().revision, 4);

    session.disconnect().unwrap();
    for tap in MonitorTap::ALL {
        assert!(session.latest_monitor_frame(tap).is_none());
    }
    server_thread.join().unwrap();
}

#[test]
fn monitor_frame_sequence_must_strictly_increase_per_tap() {
    let mut records = monitor_frame(MonitorTap::Preview, 5, 2, 2);
    // The other tap keeps its own sequence space.
    records.extend(monitor_frame(MonitorTap::Program, 1, 2, 2));
    records.extend(monitor_frame(MonitorTap::Preview, 5, 2, 2));
    let (mut session, server_thread) = connect_with_records(records);
    for tap in [MonitorTap::Preview, MonitorTap::Program] {
        assert_eq!(
            session.receive().unwrap(),
            SessionEvent::MonitorFrame {
                tap,
                complete: true
            }
        );
    }
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();
}

#[test]
fn monitor_frame_rejects_foreign_server_identity() {
    let mut foreign = monitor_part(&monitor_frame(MonitorTap::Program, 1, 2, 2)[0]);
    foreign.server.state_epoch += 1;
    let (mut session, server_thread) =
        connect_with_records(vec![WireMessage::MonitorFrame(foreign)]);
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();
}

#[test]
fn monitor_frame_groups_are_atomic_on_the_wire() {
    let program = monitor_frame(MonitorTap::Program, 1, 200, 100);
    let preview = monitor_frame(MonitorTap::Preview, 1, 2, 2);

    // Another record inside a started group.
    let records = vec![
        program[0].clone(),
        WireMessage::AudioMeters(audio_meters(1)),
    ];
    let (mut session, server_thread) = connect_with_records(records);
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::MonitorFrame {
            complete: false,
            ..
        }
    ));
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();

    // The other tap inside a started group.
    let records = vec![program[0].clone(), preview[0].clone()];
    let (mut session, server_thread) = connect_with_records(records);
    session.receive().unwrap();
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();

    // A skipped part.
    let records = vec![program[0].clone(), program[2].clone()];
    let (mut session, server_thread) = connect_with_records(records);
    session.receive().unwrap();
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();

    // A group that does not start at part zero.
    let (mut session, server_thread) = connect_with_records(vec![program[1].clone()]);
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();

    // A part whose header differs from its group.
    let mut changed = monitor_part(&program[1]);
    changed.frame += 1;
    let records = vec![program[0].clone(), WireMessage::MonitorFrame(changed)];
    let (mut session, server_thread) = connect_with_records(records);
    session.receive().unwrap();
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();
}

#[test]
fn monitor_frame_with_invalid_qoi_disconnects() {
    let mut corrupt = monitor_part(&monitor_frame(MonitorTap::Preview, 1, 2, 2)[0]);
    corrupt.data[0] = b'Q';
    let earlier = monitor_frame(MonitorTap::Program, 1, 2, 2);
    let mut records = earlier;
    records.push(WireMessage::MonitorFrame(corrupt));
    let (mut session, server_thread) = connect_with_records(records);
    session.receive().unwrap();
    assert!(session.latest_monitor_frame(MonitorTap::Program).is_some());
    assert_monitor_violation(&mut session);
    server_thread.join().unwrap();
}

/// Protocol 2.42: a display inventory request goes out on the session and
/// its reply arrives as a session event that updates the replicated view.
#[test]
fn display_inventory_request_round_trips_into_the_view() {
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        let WireMessage::DisplayInventoryRequest(request) = peer.receive() else {
            panic!("expected a display inventory request");
        };
        assert_eq!(request.request_id, "displays-1");
        peer.send_all(&[WireMessage::DisplayInventory(
            fm_protocol::DisplayInventoryMessage {
                request_id: request.request_id,
                inventory: fm_protocol::DisplayInventory {
                    presenter: false,
                    error: None,
                    displays: Vec::new(),
                },
            },
        )]);
    });
    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session
        .send_display_inventory_request(fm_protocol::DisplayInventoryRequest {
            request_id: "displays-1".into(),
        })
        .unwrap();
    let event = session.receive().unwrap();
    let SessionEvent::DisplayInventory { message } = event else {
        panic!("expected the display inventory, got {event:?}");
    };
    assert_eq!(message.request_id, "displays-1");
    let inventory = session
        .client()
        .model()
        .view()
        .unwrap()
        .display_inventory
        .unwrap();
    assert!(!inventory.presenter);
    server_thread.join().unwrap();
}

#[test]
fn diagnostics_wait_assembles_monitor_frames() {
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        let WireMessage::DiagnosticsRequest(request) = peer.receive() else {
            panic!("expected a diagnostics request");
        };
        let mut records = monitor_frame(MonitorTap::Program, 3, 200, 100);
        records.push(WireMessage::DiagnosticsResponse(DiagnosticsResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: request.request_id,
            engine: engine(),
            current_revision: 4,
            oldest_retained_revision: None,
            newest_retained_revision: None,
            subscriber_count: 1,
            retained_events_limit: 16,
            subscriber_limit: 4,
            subscriber_queue_limit: 8,
        }));
        peer.send_all(&records);
    });
    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let event = session
        .send_diagnostics_cancellable("diag-1", Duration::from_millis(5), || false)
        .unwrap();
    assert!(matches!(event, SessionEvent::DiagnosticsResponse { .. }));
    let (image, _) = session.latest_monitor_frame(MonitorTap::Program).unwrap();
    assert_eq!(image.sequence, 3);
    server_thread.join().unwrap();
}

#[test]
fn diagnostics_wait_reconciles_results_answered_before_it() {
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        let WireMessage::Command(command) = peer.receive() else {
            panic!("expected the command before the diagnostics request");
        };
        let WireMessage::DiagnosticsRequest(request) = peer.receive() else {
            panic!("expected a diagnostics request");
        };
        peer.send_all(&[
            WireMessage::CommandResult(CommandResult::Accepted {
                id: command.id,
                revision: 5,
                scheduled_frame: Some(9),
            }),
            WireMessage::DiagnosticsResponse(DiagnosticsResponse {
                protocol: CURRENT_PROTOCOL_VERSION,
                request_id: request.request_id,
                engine: engine(),
                current_revision: 5,
                oldest_retained_revision: None,
                newest_retained_revision: None,
                subscriber_count: 1,
                retained_events_limit: 16,
                subscriber_limit: 4,
                subscriber_queue_limit: 8,
            }),
        ]);
    });
    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    let command = session
        .queue_command(CommandPayload::Cut, "cut-before-diagnostics", Some(4), None)
        .unwrap();
    assert_eq!(session.flush().unwrap(), 1);

    let event = session
        .send_diagnostics_cancellable("diag-1", Duration::from_millis(5), || false)
        .unwrap();
    assert!(matches!(event, SessionEvent::DiagnosticsResponse { .. }));
    assert!(session.is_connected());
    assert_eq!(session.in_flight_len(), 0);
    let client = session.into_client();
    assert!(matches!(
        client.command(&command.id).unwrap().status,
        CommandStatus::Completed(CommandResult::Accepted { revision: 5, .. })
    ));
    server_thread.join().unwrap();
}

/// Protocol 2.29: a title preview request goes out as one record and its
/// `title_preview_frame` parts assemble into the latest preview; an
/// interleaved record breaks the atomic group.
#[test]
fn title_previews_are_requested_assembled_and_atomic() {
    use fm_protocol::{
        TitlePreviewRequest, TitlePreviewSample, encode_qoi_rgba, title_preview_parts,
    };

    let width = 300_u16;
    let height = 200_u16;
    let rgba: Vec<u8> = (0..u32::from(width) * u32::from(height))
        .flat_map(|index| {
            let [a, b, c, d] = index.wrapping_mul(2_654_435_761).to_le_bytes();
            [a, b, c, d | 1]
        })
        .collect();
    let qoi = encode_qoi_rgba(width, height, &rgba).unwrap();
    let parts: Vec<WireMessage> = title_preview_parts(
        &server(),
        "anim-preview-3",
        input(2),
        4,
        true,
        width,
        height,
        &qoi,
    )
    .into_iter()
    .map(WireMessage::TitlePreviewFrame)
    .collect();
    assert!(parts.len() > 1, "the test preview must span several parts");
    let request = TitlePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "anim-preview-3".into(),
        input: input(2),
        sample: TitlePreviewSample::In { frame: 5 },
        width,
        height,
        clip: None,
    };

    let (release_tx, release_rx) = mpsc::channel();
    let expected_request = request.clone();
    let records = parts.clone();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        assert_eq!(
            peer.receive(),
            WireMessage::TitlePreviewRequest(expected_request)
        );
        release_rx.recv().unwrap();
        let mut interleaved = records.clone();
        interleaved.insert(1, WireMessage::AudioMeters(audio_meters(1)));
        peer.send_all(&records);
        peer.send_all(&interleaved);
    });
    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session.send_title_preview(request).unwrap();
    release_tx.send(()).unwrap();

    for index in 0..parts.len() {
        let complete = index + 1 == parts.len();
        assert_eq!(
            session.receive().unwrap(),
            SessionEvent::TitlePreview {
                request_id: "anim-preview-3".into(),
                complete,
            }
        );
        assert_eq!(session.latest_title_preview().is_some(), complete);
    }
    let (image, _) = session.latest_title_preview().unwrap();
    assert_eq!(
        (image.input, image.animation_revision, image.draft),
        (input(2), 4, true)
    );
    assert_eq!((image.width, image.height), (width, height));
    assert_eq!(&*image.rgba, rgba.as_slice());

    // The second group is broken by an audio meter record.
    assert!(matches!(
        session.receive().unwrap(),
        SessionEvent::TitlePreview {
            complete: false,
            ..
        }
    ));
    assert!(matches!(
        session.receive(),
        Err(TcpSessionError::UnexpectedMessage)
    ));
    assert!(matches!(
        session.client().state(),
        ConnectionState::Backoff(_)
    ));
    assert!(session.latest_title_preview().is_none());
    server_thread.join().unwrap();
}

/// IN-001, Protocol 2.47: a capture devices request goes out exactly and
/// its reply surfaces as [`SessionEvent::CaptureDevices`] and in the model.
#[test]
fn capture_devices_request_and_reply_round_trip() {
    use fm_protocol::{CaptureDevicesMessage, CaptureDevicesRequest, CaptureInventory};

    let inventory = CaptureInventory {
        backend: Some("fake".into()),
        error: Some("one device busy".into()),
        devices: Vec::new(),
    };
    let reply = inventory.clone();
    let (address, server_thread) = spawn_server(move |listener| {
        let mut peer = Peer::accept(&listener);
        accept_snapshot(&mut peer, 4);
        let WireMessage::CaptureDevicesRequest(request) = peer.receive() else {
            panic!("expected a capture devices request")
        };
        assert_eq!(request.request_id, "devices-1");
        peer.send(&WireMessage::CaptureDevices(CaptureDevicesMessage {
            request_id: request.request_id,
            inventory: reply,
        }));
    });

    let mut session = TcpSession::new(client(4));
    session.connect(address, CONNECT_TIMEOUT).unwrap();
    session
        .send_capture_devices_request(CaptureDevicesRequest {
            request_id: "devices-1".into(),
        })
        .unwrap();
    assert_eq!(
        session.receive().unwrap(),
        SessionEvent::CaptureDevices {
            message: CaptureDevicesMessage {
                request_id: "devices-1".into(),
                inventory: inventory.clone(),
            }
        }
    );
    assert_eq!(
        session.client().model().capture_inventory(),
        Some(&inventory)
    );
    server_thread.join().unwrap();
}
