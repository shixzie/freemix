use core::num::NonZeroU128;

use fm_client::{
    Client, ClientConfig, ClientError, CommandStatus, CommandUncertainty, ConnectionState,
    DEFAULT_COMPLETED_COMMAND_CAPACITY, Intake, MAX_COMPLETED_COMMAND_CAPACITY, Outbound, SyncMode,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CapabilityReportSummary, ClientType, CommandPayload, CommandResult,
    EngineIdentity, EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    HandshakeOutcome, HandshakeResponse, ManualTransitionKind, ManualTransitionPosition,
    ManualTransitionState, ManualTransitionStatus, ProtocolVersion, RecordingStatus, ResumeCursor,
    Role, RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage,
    SnapshotReason, StingerAudioPolicy, StingerMissingMediaFallback, StingerReadiness,
    StingerStatus, WireInputId, WireMessage, WireRecorderSlot, WireStingerSlotId,
};
use fm_types::ProjectId;
use fm_ui_model::ManualTransitionStatus as ModelManualTransitionStatus;

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

fn recorder_slot(value: u8) -> WireRecorderSlot {
    WireRecorderSlot::new(value).unwrap()
}

fn recordings() -> Vec<RecordingStatus> {
    vec![
        RecordingStatus {
            recorder: recorder_slot(1),
            desired_active: false,
        },
        RecordingStatus {
            recorder: recorder_slot(2),
            desired_active: false,
        },
    ]
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

fn config(capacity: usize) -> ClientConfig {
    let mut config = ClientConfig::new(
        "diagnostic 0.1",
        ClientType::Cli,
        Role::Operator,
        "diagnostic-a",
        project_id(),
    );
    config.outbound_capacity = capacity;
    config.initial_backoff_ms = 10;
    config.max_backoff_ms = 40;
    config
}

fn handshake(revision: u64, resume: Option<ResumeCursor>) -> HandshakeResponse {
    handshake_version(CURRENT_PROTOCOL_VERSION, revision, resume)
}

fn handshake_version(
    protocol: ProtocolVersion,
    revision: u64,
    resume: Option<ResumeCursor>,
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
        outcome: resume.map_or(
            HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
            |cursor| HandshakeOutcome::Resume { cursor },
        ),
    }
}

fn snapshot(revision: u64) -> SnapshotMessage {
    SnapshotMessage {
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
        desired_fade_to_black: live_fade_to_black(),
        realized_fade_to_black: live_fade_to_black(),
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: recordings(),
        desired_overlays: fm_protocol::OverlayStatus::empty_channels(),
        realized_overlays: fm_protocol::OverlayStatus::empty_channels(),
        edit_locks: fm_protocol::EditLockStatus::default(),
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        titles: Vec::new(),
    }
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

fn live_fade_to_black() -> FadeToBlackState {
    fade_to_black(false, FadeToBlackPosition::LIVE.numerator())
}

fn fade_to_black(target_active: bool, numerator: u16) -> FadeToBlackState {
    FadeToBlackState {
        target_active,
        position: FadeToBlackPosition::new(numerator),
    }
}

fn connect_snapshot(client: &mut Client, revision: u64) {
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(revision, None)).unwrap();
    client.apply_snapshot(snapshot(revision)).unwrap();
}

fn ready_client(capacity: usize) -> Client {
    let mut client = Client::new(config(capacity)).unwrap();
    connect_snapshot(&mut client, 4);
    client
}

fn active_manual(interval_start: u16, position: u16) -> ManualTransitionStatus {
    ManualTransitionStatus::Active(ManualTransitionState {
        kind: ManualTransitionKind::Wipe,
        from: input(1),
        to: input(2),
        interval_start: ManualTransitionPosition::new(interval_start).unwrap(),
        position: ManualTransitionPosition::new(position).unwrap(),
    })
}

fn complete_cut(client: &mut Client, key: String) -> fm_protocol::CommandMessage {
    let command = client
        .queue_command(CommandPayload::Cut, key, Some(4), None)
        .unwrap();
    assert!(matches!(
        client.pop_outbound(),
        Some(Outbound::Command(queued)) if queued == command
    ));
    client
        .reconcile_result(CommandResult::Accepted {
            id: command.id.clone(),
            revision: 4,
            scheduled_frame: None,
        })
        .unwrap();
    command
}

fn runtime_event(
    revision: u64,
    generation: u64,
    sequence: u64,
    event: RuntimeLifecycleEvent,
) -> RuntimeEventMessage {
    RuntimeEventMessage {
        server: server(),
        revision,
        generation,
        sequence,
        event,
    }
}

#[test]
fn connect_snapshot_resume_and_reconnect_are_deterministic() {
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    let first_hello = client.transport_connected().unwrap();
    assert_eq!(first_hello.resume_cursor, None);
    client.accept_handshake(handshake(4, None)).unwrap();
    assert_eq!(
        client.state(),
        &ConnectionState::Synchronizing {
            mode: SyncMode::Snapshot,
            target_revision: 4
        }
    );
    client.apply_snapshot(snapshot(4)).unwrap();
    assert_eq!(client.state(), &ConnectionState::Ready);

    let backoff = client.transport_disconnected();
    assert_eq!(backoff.attempt, 1);
    assert_eq!(backoff.delay_ms, 10);
    client.start_connect().unwrap();
    let reconnect_hello = client.transport_connected().unwrap();
    let cursor = reconnect_hello.resume_cursor.unwrap();
    assert_eq!(cursor.revision, 4);
    client.accept_handshake(handshake(5, Some(cursor))).unwrap();
    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(2),
                preview: input(1),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        })
        .unwrap();
    assert_eq!(client.state(), &ConnectionState::Ready);
    assert_eq!(
        client.model().view().unwrap().switcher.desired.program,
        input(2).to_domain()
    );

    assert_eq!(client.transport_disconnected().attempt, 1);
    assert_eq!(client.transport_disconnected().attempt, 2);
    assert_eq!(client.transport_disconnected().delay_ms, 40);
}

#[test]
fn current_contract_reduces_exact_fade_to_black_state() {
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(4, None)).unwrap();

    let mut initial = snapshot(4);
    initial.desired_fade_to_black = fade_to_black(true, 40_000);
    initial.realized_fade_to_black = fade_to_black(true, 20_000);
    client.apply_snapshot(initial).unwrap();
    let switcher = client.model().state().unwrap().switcher();
    assert_eq!(switcher.desired_fade_to_black, fade_to_black(true, 40_000));
    assert_eq!(switcher.realized_fade_to_black, fade_to_black(true, 20_000));

    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: fade_to_black(false, 12_345),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        })
        .unwrap();
    assert_eq!(
        client
            .model()
            .state()
            .unwrap()
            .switcher()
            .desired_fade_to_black,
        fade_to_black(false, 12_345)
    );

    client
        .apply_runtime_event(runtime_event(
            5,
            7,
            1,
            RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: fade_to_black(false, 12_345),
            },
        ))
        .unwrap();
    assert_eq!(
        client
            .model()
            .state()
            .unwrap()
            .switcher()
            .realized_fade_to_black,
        fade_to_black(false, 12_345)
    );
}

#[test]
fn current_contract_reduces_exact_stinger_status() {
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(4, None)).unwrap();

    let status = StingerStatus {
        slot: WireStingerSlotId::new(8).unwrap(),
        media_input: input(2),
        preload: true,
        cut_point_frames: 7,
        audio_policy: StingerAudioPolicy::StingerOnly,
        missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
        readiness: StingerReadiness::Missing,
    };
    let mut complete = snapshot(4);
    complete.stingers = vec![status];
    client.apply_snapshot(complete).unwrap();
    let projected = &client.model().view().unwrap().stingers;
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].slot, 8);
    assert_eq!(projected[0].media_input, input(2).to_domain());
    assert_eq!(projected[0].readiness, StingerReadiness::Missing);
}

#[test]
fn protocol_mismatch_is_terminal_until_configuration_changes() {
    let mut client = Client::new(config(2)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    let mut mismatched = handshake(0, None);
    mismatched.protocol = ProtocolVersion::new(99, 0);
    assert_eq!(
        client.accept_handshake(mismatched),
        Err(ClientError::ProtocolMismatch(ProtocolVersion::new(99, 0)))
    );
    assert_eq!(
        client.state(),
        &ConnectionState::ProtocolMismatch {
            protocol: ProtocolVersion::new(99, 0)
        }
    );
}

#[test]
fn completed_history_configuration_requires_a_finite_nonzero_bound() {
    assert_eq!(
        config(1).completed_command_capacity,
        DEFAULT_COMPLETED_COMMAND_CAPACITY
    );

    let mut zero = config(1);
    zero.completed_command_capacity = 0;
    assert!(matches!(
        Client::new(zero),
        Err(ClientError::InvalidConfig(
            "completed command capacity must be finite and between 1 and 65536"
        ))
    ));

    let mut unbounded = config(1);
    unbounded.completed_command_capacity = usize::MAX;
    assert!(unbounded.completed_command_capacity > MAX_COMPLETED_COMMAND_CAPACITY);
    assert!(matches!(
        Client::new(unbounded),
        Err(ClientError::InvalidConfig(
            "completed command capacity must be finite and between 1 and 65536"
        ))
    ));
}

#[test]
fn completed_history_stays_constant_and_reconnect_scan_is_bounded() {
    const RETAINED: usize = 32;
    const COMPLETED: usize = 4_096;

    let mut settings = config(1);
    settings.completed_command_capacity = RETAINED;
    let mut client = Client::new(settings).unwrap();
    connect_snapshot(&mut client, 4);

    for sequence in 1..=COMPLETED {
        complete_cut(&mut client, format!("stress-{sequence}"));
        assert!(client.retained_command_count() <= RETAINED);
    }
    assert_eq!(client.retained_command_count(), RETAINED);
    assert!(client.command("diagnostic-a:4064").is_none());
    assert!(matches!(
        client.command("diagnostic-a:4065").unwrap().status,
        CommandStatus::Completed(_)
    ));
    assert!(matches!(
        client.command("diagnostic-a:4096").unwrap().status,
        CommandStatus::Completed(_)
    ));

    let _ = client.transport_disconnected();
    client.start_connect().unwrap();
    let cursor = client.transport_connected().unwrap().resume_cursor.unwrap();
    client
        .accept_handshake(handshake_version(CURRENT_PROTOCOL_VERSION, 4, Some(cursor)))
        .unwrap();
    assert_eq!(client.state(), &ConnectionState::Ready);
    assert_eq!(client.retained_command_count(), RETAINED);
    assert_eq!(
        client
            .command("diagnostic-a:4096")
            .unwrap()
            .command
            .protocol,
        CURRENT_PROTOCOL_VERSION
    );
}

#[test]
fn completed_eviction_preserves_all_unresolved_state_and_uncertainty() {
    let mut settings = config(2);
    settings.completed_command_capacity = 2;
    let mut client = Client::new(settings).unwrap();
    connect_snapshot(&mut client, 4);

    let wipe = client
        .queue_command(
            CommandPayload::Wipe {
                duration_frames: 45,
            },
            "unresolved-wipe",
            Some(4),
            None,
        )
        .unwrap();
    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(_))));
    let preview = client
        .queue_command(
            CommandPayload::SelectPreview { input: input(1) },
            "unresolved-preview",
            Some(4),
            None,
        )
        .unwrap();
    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(_))));

    for sequence in 0..128 {
        complete_cut(&mut client, format!("completed-{sequence}"));
    }
    assert_eq!(client.retained_command_count(), 4);
    assert_eq!(client.command(&wipe.id).unwrap().command, wipe);
    assert_eq!(
        client.command(&wipe.id).unwrap().status,
        CommandStatus::Sent
    );
    assert_eq!(client.command(&preview.id).unwrap().command, preview);
    assert_eq!(
        client.command(&preview.id).unwrap().status,
        CommandStatus::Sent
    );
    assert_eq!(client.model().pending_commands().len(), 2);
    assert_eq!(
        client.model().view().unwrap().switcher.desired.preview,
        input(1).to_domain()
    );

    let _ = client.transport_disconnected();
    client.start_connect().unwrap();
    let cursor = client.transport_connected().unwrap().resume_cursor.unwrap();
    client
        .accept_handshake(handshake_version(CURRENT_PROTOCOL_VERSION, 4, Some(cursor)))
        .unwrap();
    assert_eq!(client.retained_command_count(), 4);
    assert_eq!(client.command(&wipe.id).unwrap().command, wipe);
    assert_eq!(
        client.command(&preview.id).unwrap().command.protocol,
        CURRENT_PROTOCOL_VERSION
    );
    assert_eq!(client.model().pending_commands().len(), 2);
    assert_eq!(
        client.queue_command(CommandPayload::Cut, "unresolved-wipe", Some(4), None),
        Err(ClientError::DuplicateIdempotencyKey(
            "unresolved-wipe".to_owned()
        ))
    );
}

#[test]
fn completion_order_evicts_command_and_local_key_index_together() {
    let mut settings = config(3);
    settings.completed_command_capacity = 2;
    let mut client = Client::new(settings).unwrap();
    connect_snapshot(&mut client, 4);

    let commands = ["first", "second", "third"].map(|key| {
        let command = client
            .queue_command(CommandPayload::Cut, key, Some(4), None)
            .unwrap();
        client.pop_outbound();
        command
    });
    client.retry_command(&commands[2].id).unwrap();
    assert_eq!(client.outbound_len(), 1);
    for index in [2, 0, 1] {
        client
            .reconcile_result(CommandResult::Accepted {
                id: commands[index].id.clone(),
                revision: 4,
                scheduled_frame: None,
            })
            .unwrap();
        if index == 2 {
            assert_eq!(client.outbound_len(), 0);
        }
    }

    assert!(client.command(&commands[2].id).is_none());
    assert!(client.command(&commands[0].id).is_some());
    assert!(client.command(&commands[1].id).is_some());
    assert_eq!(client.retained_command_count(), 2);
    // The bounded local index forgets the key; this does not make reuse safe
    // while the server may still retain its original receipt.
    let locally_accepted = client
        .queue_command(CommandPayload::Cut, "third", Some(4), None)
        .unwrap();
    assert_eq!(locally_accepted.id, "diagnostic-a:4");
    assert_eq!(
        client.queue_command(CommandPayload::Cut, "first", Some(4), None),
        Err(ClientError::DuplicateIdempotencyKey("first".to_owned()))
    );
}

#[test]
fn replayed_evicted_receipt_terminally_fails_sent_commands_and_forces_snapshot() {
    let mut settings = config(2);
    settings.completed_command_capacity = 2;
    let mut client = Client::new(settings).unwrap();
    connect_snapshot(&mut client, 4);

    let old = complete_cut(&mut client, "globally-reused-key".to_owned());
    complete_cut(&mut client, "evictor-a".to_owned());
    complete_cut(&mut client, "evictor-b".to_owned());
    assert!(client.command(&old.id).is_none());
    assert_eq!(client.retained_command_count(), 2);

    let collision = client
        .queue_command(
            CommandPayload::SelectPreview { input: input(1) },
            "globally-reused-key",
            Some(4),
            None,
        )
        .unwrap();
    let collateral = client
        .queue_command(CommandPayload::Cut, "still-unique", Some(4), None)
        .unwrap();
    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(_))));
    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(_))));
    assert_eq!(client.model().pending_commands().len(), 2);
    assert_eq!(
        client.model().view().unwrap().switcher.desired.preview,
        input(1).to_domain()
    );

    let affected = vec![collision.id.clone(), collateral.id.clone()];
    let error = client
        .reconcile_result(CommandResult::Accepted {
            id: old.id.clone(),
            revision: 4,
            scheduled_frame: None,
        })
        .unwrap_err();
    assert_eq!(
        error,
        ClientError::IdempotencyReplayCollision {
            received_command_id: old.id.clone(),
            affected_command_ids: affected.clone(),
        }
    );
    assert!(
        error
            .to_string()
            .contains("authoritative snapshot is required")
    );
    assert_eq!(
        client.state(),
        &ConnectionState::ResyncRequired {
            expected_revision: 5,
            received_revision: 4,
        }
    );
    assert_eq!(client.retained_command_count(), 2);
    assert_eq!(client.outbound_len(), 0);
    assert_eq!(client.model().pending_commands(), []);
    assert_eq!(
        client.model().view().unwrap().switcher.desired.preview,
        input(2).to_domain()
    );
    for id in &affected {
        assert_eq!(
            client.command(id).unwrap().status,
            CommandStatus::TerminalUncertain(CommandUncertainty::IdempotencyReplayCollision {
                received_command_id: old.id.clone(),
            })
        );
    }

    client.start_connect().unwrap();
    assert_eq!(client.transport_connected().unwrap().resume_cursor, None);
    client.accept_handshake(handshake(4, None)).unwrap();
    client.apply_snapshot(snapshot(4)).unwrap();
    assert_eq!(client.state(), &ConnectionState::Ready);
    assert_eq!(client.retained_command_count(), 2);
    assert_eq!(
        client.retry_command(&collision.id),
        Err(ClientError::CommandTerminalUncertain(collision.id))
    );
}

#[test]
fn commands_have_monotonic_ids_and_results_reconcile_through_ui_model() {
    let mut client = ready_client(4);
    let first = client
        .queue_command(
            CommandPayload::SelectPreview { input: input(1) },
            "intent-a",
            Some(4),
            Some(500),
        )
        .unwrap();
    let second = client
        .queue_command(CommandPayload::Cut, "intent-b", None, None)
        .unwrap();
    assert_eq!(first.id, "diagnostic-a:1");
    assert_eq!(second.id, "diagnostic-a:2");
    assert_eq!(first.expected_revision, Some(4));
    assert_eq!(first.idempotency_key, "intent-a");
    assert_eq!(client.model().pending_commands().len(), 2);
    assert_eq!(
        client.model().view().unwrap().switcher.desired.preview,
        input(1).to_domain()
    );

    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(_))));
    assert_eq!(
        client.command(&first.id).unwrap().status,
        CommandStatus::Sent
    );
    let result = CommandResult::Accepted {
        id: first.id.clone(),
        revision: 5,
        scheduled_frame: Some(9),
    };
    assert_eq!(
        client.reconcile_result(result.clone()).unwrap(),
        Intake::ResultReconciled
    );
    assert_eq!(
        client.command(&first.id).unwrap().status,
        CommandStatus::Completed(result.clone())
    );
    assert_eq!(
        client.reconcile_result(result).unwrap(),
        Intake::DuplicateResult
    );
}

#[test]
fn whitespace_only_idempotency_key_does_not_mutate_command_state() {
    let mut client = ready_client(4);

    let error = client
        .queue_command(CommandPayload::Cut, " \t\n", None, None)
        .unwrap_err();
    assert_eq!(error, ClientError::EmptyIdempotencyKey);
    assert_eq!(error.to_string(), "idempotency key must not be blank");
    assert_eq!(client.outbound_len(), 0);
    assert_eq!(client.retained_command_count(), 0);
    assert_eq!(client.model().pending_commands(), []);

    let command = client
        .queue_command(CommandPayload::Cut, " accepted ", None, None)
        .unwrap();
    assert_eq!(command.id, "diagnostic-a:1");
    assert_eq!(command.idempotency_key, " accepted ");
}

#[test]
fn wipe_command_is_queued_with_its_exact_duration() {
    let mut client = ready_client(1);
    let command = client
        .queue_command(
            CommandPayload::Wipe {
                duration_frames: 45,
            },
            "wipe-45",
            Some(4),
            None,
        )
        .unwrap();

    assert_eq!(
        command.payload,
        CommandPayload::Wipe {
            duration_frames: 45,
        }
    );
    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(queued)) if queued == command));
}

#[test]
fn event_gap_requests_snapshot_resync() {
    let mut client = ready_client(2);
    let error = client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 6,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(2),
                preview: input(1),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        })
        .unwrap_err();
    assert_eq!(
        error,
        ClientError::ResyncRequired {
            expected_revision: 5,
            received_revision: 6
        }
    );
    assert!(matches!(
        client.state(),
        ConnectionState::ResyncRequired { .. }
    ));
    client.start_connect().unwrap();
    assert_eq!(client.transport_connected().unwrap().resume_cursor, None);
}

#[test]
fn same_revision_runtime_realization_does_not_move_durable_cursor() {
    let mut client = ready_client(2);
    let cursor = client.last_applied_cursor().unwrap();

    assert_eq!(
        client
            .intake(WireMessage::RuntimeEvent(runtime_event(
                4,
                7,
                1,
                RuntimeLifecycleEvent::Realized {
                    domain: "switcher".to_owned(),
                    manual_transition: ManualTransitionStatus::Inactive,
                    fade_to_black: live_fade_to_black(),
                },
            )))
            .unwrap(),
        Intake::RuntimeEventObserved
    );

    assert_eq!(client.last_applied_cursor(), Some(cursor));
    assert_eq!(
        client
            .model()
            .state()
            .unwrap()
            .switcher()
            .runtime_generation,
        Some(7)
    );
}

#[test]
fn reconnecting_second_client_reduces_exact_desired_and_realized_manual_state() {
    let mut first = Client::new(config(4)).unwrap();
    first.start_connect().unwrap();
    first.transport_connected().unwrap();
    first.accept_handshake(handshake(4, None)).unwrap();
    let mut initial = snapshot(4);
    initial.desired_manual_transition = active_manual(0, 6_250);
    initial.realized_manual_transition = active_manual(6_250, 6_250);
    first.apply_snapshot(initial.clone()).unwrap();

    let mut second = Client::new(config(4)).unwrap();
    second.start_connect().unwrap();
    second.transport_connected().unwrap();
    second.accept_handshake(handshake(4, None)).unwrap();
    second.apply_snapshot(initial).unwrap();
    let switcher = second.model().state().unwrap().switcher();
    assert!(matches!(
        switcher.desired_manual_transition,
        ModelManualTransitionStatus::Active(state)
            if state.interval_start.basis_points() == 0
                && state.position.basis_points() == 6_250
    ));
    assert!(matches!(
        switcher.realized_manual_transition,
        ModelManualTransitionStatus::Active(state)
            if state.interval_start.basis_points() == 6_250
                && state.position.basis_points() == 6_250
    ));

    second
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: active_manual(0, 2_500),
                fade_to_black: live_fade_to_black(),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        })
        .unwrap();
    second
        .apply_runtime_event(runtime_event(
            5,
            5,
            1,
            RuntimeLifecycleEvent::Realized {
                domain: "switcher".into(),
                manual_transition: active_manual(2_500, 2_500),
                fade_to_black: live_fade_to_black(),
            },
        ))
        .unwrap();
    let switcher = second.model().state().unwrap().switcher();
    assert!(matches!(
        switcher.desired_manual_transition,
        ModelManualTransitionStatus::Active(state)
            if state.position.basis_points() == 2_500
    ));
    assert!(matches!(
        switcher.realized_manual_transition,
        ModelManualTransitionStatus::Active(state)
            if state.interval_start.basis_points() == 2_500
                && state.position.basis_points() == 2_500
    ));
}

#[test]
fn runtime_lifecycle_sequence_is_recorded_without_durable_reduction() {
    let mut client = ready_client(2);
    client
        .apply_runtime_event(runtime_event(4, 9, 1, RuntimeLifecycleEvent::Preparing))
        .unwrap();
    client
        .apply_runtime_event(runtime_event(
            4,
            9,
            2,
            RuntimeLifecycleEvent::Realized {
                domain: "audio".to_owned(),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        ))
        .unwrap();

    assert_eq!(client.last_applied_cursor().unwrap().revision, 4);
    assert_eq!(
        client
            .model()
            .state()
            .unwrap()
            .switcher()
            .runtime_generation,
        None
    );
}

#[test]
fn runtime_event_rejects_wrong_identity_and_revision_ahead() {
    let mut client = ready_client(2);
    let mut wrong_identity = runtime_event(4, 1, 1, RuntimeLifecycleEvent::Accepted);
    wrong_identity.server.engine_id = "engine-b".to_owned();
    assert_eq!(
        client.apply_runtime_event(wrong_identity.clone()),
        Err(ClientError::RuntimeIdentityMismatch {
            expected: Box::new(server()),
            received: Box::new(wrong_identity.server),
        })
    );
    assert_eq!(
        client.apply_runtime_event(runtime_event(5, 1, 1, RuntimeLifecycleEvent::Accepted,)),
        Err(ClientError::RuntimeRevisionAhead {
            current_revision: 4,
            received_revision: 5,
        })
    );
    assert_eq!(client.state(), &ConnectionState::Ready);
    assert_eq!(client.last_applied_cursor().unwrap().revision, 4);
}

#[test]
fn runtime_sequence_gap_does_not_request_durable_resync() {
    let mut client = ready_client(2);
    client
        .apply_runtime_event(runtime_event(4, 3, 1, RuntimeLifecycleEvent::Accepted))
        .unwrap();
    assert_eq!(
        client.apply_runtime_event(runtime_event(4, 3, 3, RuntimeLifecycleEvent::Preparing,)),
        Err(ClientError::RuntimeSequenceGap {
            generation: 3,
            expected_sequence: 2,
            received_sequence: 3,
        })
    );
    assert_eq!(client.state(), &ConnectionState::Ready);
    assert_eq!(client.last_applied_cursor().unwrap().revision, 4);

    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(2),
                preview: input(1),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        })
        .unwrap();
    assert_eq!(client.last_applied_cursor().unwrap().revision, 5);
}

#[test]
fn runtime_sequences_are_per_generation_and_reset_on_disconnect() {
    let mut client = ready_client(2);
    client
        .apply_runtime_event(runtime_event(4, 1, 1, RuntimeLifecycleEvent::Accepted))
        .unwrap();
    client
        .apply_runtime_event(runtime_event(4, 2, 1, RuntimeLifecycleEvent::Accepted))
        .unwrap();

    let _ = client.transport_disconnected();
    client.start_connect().unwrap();
    let cursor = client.transport_connected().unwrap().resume_cursor.unwrap();
    client.accept_handshake(handshake(4, Some(cursor))).unwrap();
    client
        .apply_runtime_event(runtime_event(4, 1, 1, RuntimeLifecycleEvent::Accepted))
        .unwrap();
}

/// The server numbers a generation's runtime events across all connections, so
/// a client that joins mid-generation first observes a sequence above one.
#[test]
fn runtime_sequence_joined_mid_generation_is_a_baseline_not_a_gap() {
    let mut client = ready_client(2);
    client
        .apply_runtime_event(runtime_event(4, 5, 2, RuntimeLifecycleEvent::Accepted))
        .unwrap();
    assert_eq!(
        client.apply_runtime_event(runtime_event(4, 5, 4, RuntimeLifecycleEvent::Preparing)),
        Err(ClientError::RuntimeSequenceGap {
            generation: 5,
            expected_sequence: 3,
            received_sequence: 4,
        })
    );
    client
        .apply_runtime_event(runtime_event(4, 5, 3, RuntimeLifecycleEvent::Preparing))
        .unwrap();

    // A generation that starts on this connection starts at one.
    assert_eq!(
        client.apply_runtime_event(runtime_event(4, 6, 2, RuntimeLifecycleEvent::Accepted)),
        Err(ClientError::RuntimeSequenceGap {
            generation: 6,
            expected_sequence: 1,
            received_sequence: 2,
        })
    );
    client
        .apply_runtime_event(runtime_event(4, 6, 1, RuntimeLifecycleEvent::Accepted))
        .unwrap();

    // Generations only advance; an older one is not tracked again.
    assert_eq!(
        client.apply_runtime_event(runtime_event(4, 2, 1, RuntimeLifecycleEvent::Accepted)),
        Err(ClientError::RuntimeGenerationRegressed {
            current_generation: 6,
            received_generation: 2,
        })
    );
    assert_eq!(client.state(), &ConnectionState::Ready);
}

#[test]
fn heartbeat_reports_only_the_last_applied_cursor() {
    let mut client = ready_client(2);
    let heartbeat = client.queue_heartbeat(1_234).unwrap();
    assert_eq!(heartbeat.server, server());
    assert_eq!(heartbeat.sequence, 1);
    assert_eq!(heartbeat.sent_at_ms, 1_234);
    let cursor = heartbeat.last_applied.unwrap();
    assert_eq!(cursor.server, server());
    assert_eq!(cursor.revision, 4);
    assert!(matches!(
        client.pop_outbound(),
        Some(Outbound::Heartbeat(_))
    ));
}

#[test]
fn outbound_queue_rejects_overflow_without_consuming_an_id() {
    let mut client = ready_client(1);
    client.queue_heartbeat(0).unwrap();
    assert_eq!(
        client.queue_command(CommandPayload::Cut, "full", Some(4), None),
        Err(ClientError::QueueFull { capacity: 1 })
    );
    client.pop_outbound();
    let command = client
        .queue_command(CommandPayload::Cut, "accepted", Some(4), None)
        .unwrap();
    assert_eq!(command.id, "diagnostic-a:1");
    assert_eq!(client.outbound_len(), 1);
}

fn wire_target(value: u128) -> fm_protocol::WireStreamTargetId {
    fm_protocol::WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
}

fn protocol_stream(value: u128, name: &str, desired_running: bool) -> fm_protocol::StreamStatus {
    fm_protocol::StreamStatus {
        target: wire_target(value),
        name: name.to_owned(),
        desired_running,
        realized: fm_protocol::StreamRealizedState::Stopped,
        detail: None,
    }
}

fn snapshot_with_streams(
    revision: u64,
    streams: Vec<fm_protocol::StreamStatus>,
) -> SnapshotMessage {
    let mut message = snapshot(revision);
    message.streams = streams;
    message
}

#[test]
fn snapshot_streams_are_validated_through_the_replicated_model() {
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(4, None)).unwrap();

    let duplicate = snapshot_with_streams(
        4,
        vec![
            protocol_stream(5, "Twitch", false),
            protocol_stream(5, "Again", true),
        ],
    );
    assert_eq!(
        client.apply_snapshot(duplicate).unwrap_err(),
        ClientError::Model(fm_ui_model::ModelError::DuplicateStreamTarget(
            NonZeroU128::new(5).unwrap()
        ))
    );
}

#[test]
fn streams_changed_events_retain_the_latest_projection() {
    let mut client = Client::new(config(4)).unwrap();
    connect_snapshot(&mut client, 4);

    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::StreamsChanged {
                streams: vec![
                    protocol_stream(5, "Twitch", true),
                    protocol_stream(9, "YouTube", false),
                ],
            },
        })
        .unwrap();
    let state = client.model().state().unwrap();
    assert_eq!(state.streams().len(), 2);
    assert_eq!(state.streams()[0].name, "Twitch");
    assert!(state.streams()[0].desired_running);
    assert_eq!(state.streams()[1].target.get(), 9);
    assert!(!state.streams()[1].desired_running);
}

#[test]
fn snapshots_intake_the_per_slot_recording_projection() {
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(4, None)).unwrap();

    let mut message = snapshot_with_streams(4, Vec::new());
    message.recordings[0].desired_active = true;
    client.apply_snapshot(message).unwrap();
    let state = client.model().state().unwrap();
    assert!(state.recording_desired(recorder_slot(1)));
    assert!(!state.recording_desired(recorder_slot(2)));
}

#[test]
fn recording_changed_events_update_only_the_addressed_slot() {
    let mut client = Client::new(config(4)).unwrap();
    connect_snapshot(&mut client, 4);

    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::RecordingChanged {
                recorder: recorder_slot(1),
                active: true,
            },
        })
        .unwrap();
    let state = client.model().state().unwrap();
    assert!(state.recording_desired(recorder_slot(1)));
    assert!(!state.recording_desired(recorder_slot(2)));

    let command = client
        .queue_command(
            CommandPayload::RecordStart {
                recorder: recorder_slot(2),
            },
            "rec-two-on",
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        command.payload,
        CommandPayload::RecordStart {
            recorder: recorder_slot(2)
        }
    );
    assert_eq!(client.model().pending_commands().len(), 1);

    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 6,
            },
            payload: EventPayload::RecordingChanged {
                recorder: recorder_slot(2),
                active: true,
            },
        })
        .unwrap();
    let state = client.model().state().unwrap();
    assert!(state.recording_desired(recorder_slot(1)));
    assert!(state.recording_desired(recorder_slot(2)));

    client
        .apply_event(EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 7,
            },
            payload: EventPayload::RecordingChanged {
                recorder: recorder_slot(1),
                active: false,
            },
        })
        .unwrap();
    let state = client.model().state().unwrap();
    assert!(!state.recording_desired(recorder_slot(1)));
    assert!(state.recording_desired(recorder_slot(2)));
}

#[test]
#[allow(clippy::too_many_lines)]
fn edit_lock_events_and_history_summaries_reduce_into_the_client_model() {
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(4, None)).unwrap();
    let mut base = snapshot(4);
    base.edit_locks = fm_protocol::EditLockStatus {
        project: true,
        inputs: vec![input(1)],
    };
    base.edit_history = fm_protocol::EditHistoryStatus {
        undo_depth: 1,
        next_undo: Some(3),
        redo_depth: 0,
        next_redo: None,
    };
    // The snapshot crosses the exact wire before it is reduced.
    let line = fm_protocol::encode_line(&WireMessage::Snapshot(base)).unwrap();
    assert_eq!(
        client
            .intake(fm_protocol::decode_line(&line).unwrap())
            .unwrap(),
        Intake::SnapshotApplied
    );
    let view = client.model().view().unwrap();
    assert!(view.edit_locks.project);
    assert_eq!(view.edit_locks.inputs, [input(1).to_domain()]);
    assert_eq!(view.edit_history.next_undo, Some(3));

    let unlocked = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: engine(),
            revision: 5,
        },
        payload: EventPayload::EditLockChanged {
            scope: fm_protocol::EditLockScope::Project,
            locked: false,
        },
        edit_history: view.edit_history,
    });
    let line = fm_protocol::encode_line(&unlocked).unwrap();
    assert_eq!(
        client
            .intake(fm_protocol::decode_line(&line).unwrap())
            .unwrap(),
        Intake::EventApplied
    );
    let locked_input = WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: engine(),
            revision: 6,
        },
        payload: EventPayload::EditLockChanged {
            scope: fm_protocol::EditLockScope::Input(input(2)),
            locked: true,
        },
        edit_history: view.edit_history,
    });
    client.intake(locked_input).unwrap();
    // An undo arrives as the edit it resolved to plus the new summary.
    client
        .intake(WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision: 7,
            },
            payload: EventPayload::InputRenamed {
                input: input(1),
                name: "Camera".to_owned(),
            },
            edit_history: fm_protocol::EditHistoryStatus {
                undo_depth: 0,
                next_undo: None,
                redo_depth: 1,
                next_redo: Some(3),
            },
        }))
        .unwrap();
    let state = client.model().state().unwrap();
    assert!(!state.edit_locks().project);
    assert_eq!(
        state.edit_locks().inputs,
        [input(1).to_domain(), input(2).to_domain()]
    );
    assert!(!state.edit_history().can_undo());
    assert_eq!(state.edit_history().next_redo, Some(3));
    assert_eq!(state.input_name(input(1).to_domain()), Some("Camera"));

    // Undo, redo, and padlock commands carry no optimistic intent.
    for payload in [
        CommandPayload::Undo {
            expected_edit: None,
        },
        CommandPayload::Redo {
            expected_edit: Some(3),
        },
        CommandPayload::SetEditLock {
            scope: fm_protocol::EditLockScope::Project,
            locked: true,
        },
    ] {
        let key = format!("{payload:?}");
        let command = client.queue_command(payload, key, None, None).unwrap();
        let pending = client
            .model()
            .pending_commands()
            .iter()
            .find(|pending| pending.id.as_str() == command.id)
            .unwrap();
        assert_eq!(pending.optimistic, None);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn input_catalogue_events_reduce_into_inputs_strips_and_tally() {
    let mut client = ready_client(4);
    // Add and remove carry no optimistic change: the catalogue moves only
    // when the durable event arrives.
    let add = client
        .queue_command(
            CommandPayload::AddInput {
                input: input(3),
                name: "Red".to_owned(),
                source: fm_protocol::LiveInputSource::Solid {
                    red: 255,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                },
            },
            "add-red".to_owned(),
            Some(4),
            None,
        )
        .unwrap();
    assert_eq!(client.model().view().unwrap().inputs.len(), 2);
    client.pop_outbound();
    client
        .reconcile_result(CommandResult::Accepted {
            id: add.id,
            revision: 5,
            scheduled_frame: Some(9),
        })
        .unwrap();

    let through_wire = |client: &mut Client, message: WireMessage| {
        let line = fm_protocol::encode_line(&message).unwrap();
        client
            .intake(fm_protocol::decode_line(&line).unwrap())
            .unwrap()
    };
    let event = |revision: u64, payload: EventPayload| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            payload,
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        })
    };
    assert_eq!(
        through_wire(
            &mut client,
            event(
                5,
                EventPayload::InputAdded {
                    input: input(3),
                    name: "Red".to_owned(),
                },
            ),
        ),
        Intake::EventApplied
    );
    let view = client.model().view().unwrap();
    assert_eq!(
        view.inputs,
        [input(1), input(2), input(3)].map(WireInputId::to_domain)
    );
    assert_eq!(view.input_names[2], "Red");
    assert_eq!(view.input_audio_strips.len(), 3);

    // Cut the new input to Program and see the tally follow the runtime.
    let mut strips = input_audio_strips();
    strips.push(fm_protocol::InputAudioStripStatus {
        input: input(3),
        ..strips[0]
    });
    let switcher = |revision: u64, program: u128, preview: u128| {
        event(
            revision,
            EventPayload::DesiredSwitcher {
                program: input(program),
                preview: input(preview),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: strips.clone(),
            },
        )
    };
    through_wire(&mut client, switcher(6, 3, 1));
    through_wire(
        &mut client,
        WireMessage::RuntimeEvent(runtime_event(
            6,
            6,
            1,
            RuntimeLifecycleEvent::Realized {
                domain: "switcher".to_owned(),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
            },
        )),
    );
    let tally = client.model().view().unwrap().switcher;
    assert_eq!(tally.desired.program, input(3).to_domain());
    assert_eq!(tally.realized.program, input(3).to_domain());

    // A removal of the input on Program would contradict the server's rule,
    // so the client refuses to apply it and asks for a fresh snapshot.
    let refused = client.intake(event(7, EventPayload::InputRemoved { input: input(3) }));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(client.model().view().unwrap().inputs.len(), 3);
    assert!(matches!(
        client.state(),
        ConnectionState::ResyncRequired { .. }
    ));
    client.start_connect().unwrap();
    assert_eq!(client.transport_connected().unwrap().resume_cursor, None);
    client.accept_handshake(handshake(7, None)).unwrap();

    // After a resync with the input off air, removal reduces cleanly.
    let mut resynced = snapshot(7);
    resynced.inputs.push(fm_protocol::InputStatus {
        input: input(3),
        name: "Red".to_owned(),
    });
    resynced.input_audio_strips = strips.clone();
    resynced.desired_program = input(2);
    resynced.realized_program = input(2);
    resynced.desired_preview = input(1);
    resynced.realized_preview = input(1);
    assert_eq!(
        through_wire(&mut client, WireMessage::Snapshot(resynced)),
        Intake::SnapshotApplied
    );
    assert_eq!(
        through_wire(
            &mut client,
            event(8, EventPayload::InputRemoved { input: input(3) })
        ),
        Intake::EventApplied
    );
    let view = client.model().view().unwrap();
    assert_eq!(
        view.inputs,
        [input(1), input(2)].map(WireInputId::to_domain)
    );
    assert_eq!(view.input_audio_strips.len(), 2);
    assert_eq!(view.switcher.realized.program, input(2).to_domain());
}

mod titles {
    use fm_protocol::{
        TitleFieldStatus, TitleFieldUpdate, TitleFieldValue, TitleStateMessage, TitleSummary,
        WireTitleFieldId,
    };

    use super::*;

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn titled_snapshot(revision: u64) -> SnapshotMessage {
        SnapshotMessage {
            titles: vec![TitleSummary {
                input: input(2),
                field_revision: 3,
            }],
            ..snapshot(revision)
        }
    }

    fn title_state(revision: u64, field_revision: u64) -> TitleStateMessage {
        TitleStateMessage {
            revision,
            input: input(2),
            name: "Lower third".into(),
            field_revision,
            fields: vec![TitleFieldStatus {
                field: field(1),
                name: "Name".into(),
                value: TitleFieldValue::Text("Ana".into()),
            }],
        }
    }

    fn synchronizing_client() -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(titled_snapshot(4)))
            .unwrap();
        client
    }

    fn title_event(revision: u64, field_revision: u64, name: &str) -> EventMessage {
        EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            payload: EventPayload::TitleChanged {
                input: input(2),
                field_revision,
                fields: vec![TitleFieldUpdate {
                    field: field(1),
                    value: TitleFieldValue::Text(name.into()),
                }],
                overlays: fm_protocol::OverlayStatus::empty_channels(),
            },
        }
    }

    #[test]
    fn snapshot_waits_for_title_states_before_ready() {
        let mut client = synchronizing_client();
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        assert!(!client.model().state().unwrap().titles_complete());

        assert_eq!(
            client
                .intake(WireMessage::TitleState(title_state(4, 3)))
                .unwrap(),
            Intake::TitleStateApplied
        );
        assert_eq!(client.state(), &ConnectionState::Ready);
        let title = client
            .model()
            .state()
            .unwrap()
            .title(input(2).to_domain())
            .unwrap();
        assert!(title.complete);
        assert_eq!(title.name, "Lower third");
        assert_eq!(title.field_revision, 3);

        client.apply_event(title_event(5, 4, "Bea")).unwrap();
        let title = client
            .model()
            .state()
            .unwrap()
            .title(input(2).to_domain())
            .unwrap();
        assert_eq!(title.field_revision, 4);
        assert_eq!(title.fields[0].value, TitleFieldValue::Text("Bea".into()));
    }

    #[test]
    fn mismatched_title_state_forces_a_fresh_snapshot() {
        let mut client = synchronizing_client();
        let error = client
            .intake(WireMessage::TitleState(title_state(4, 2)))
            .unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
        assert!(matches!(
            client.state(),
            ConnectionState::ResyncRequired { .. }
        ));
        client.start_connect().unwrap();
        assert_eq!(client.transport_connected().unwrap().resume_cursor, None);
    }

    #[test]
    fn event_before_title_states_forces_a_fresh_snapshot() {
        let mut client = synchronizing_client();
        let error = client.apply_event(title_event(5, 4, "Bea")).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn title_state_after_ready_forces_a_fresh_snapshot() {
        let mut client = synchronizing_client();
        client
            .intake(WireMessage::TitleState(title_state(4, 3)))
            .unwrap();
        let error = client
            .intake(WireMessage::TitleState(title_state(4, 3)))
            .unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn incomplete_titles_never_resume() {
        let mut client = synchronizing_client();
        let _ = client.transport_disconnected();
        client.start_connect().unwrap();
        assert_eq!(client.transport_connected().unwrap().resume_cursor, None);
    }

    #[test]
    fn title_commands_have_no_optimistic_change() {
        let mut client = synchronizing_client();
        client
            .intake(WireMessage::TitleState(title_state(4, 3)))
            .unwrap();
        let before = client.model().view().unwrap();
        for (key, payload) in [
            (
                "fields",
                CommandPayload::SetTitleFields {
                    input: input(2),
                    base_revision: 3,
                    require: fm_protocol::TitleAirRequirement::OffAir,
                    fields: Vec::new(),
                },
            ),
            (
                "take",
                CommandPayload::TakeTitle {
                    input: input(2),
                    channel: fm_protocol::WireOverlayChannelId::new(1).unwrap(),
                    base_revision: 3,
                    fields: Vec::new(),
                },
            ),
            ("off", CommandPayload::TitleOffAir { input: input(2) }),
        ] {
            client
                .queue_command(payload, key.to_owned(), Some(4), None)
                .unwrap();
        }
        assert_eq!(client.model().view().unwrap(), before);
        assert_eq!(
            client
                .model()
                .state()
                .unwrap()
                .title(input(2).to_domain())
                .unwrap()
                .field_revision,
            3
        );
    }
}
