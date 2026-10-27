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

/// The empty static `title_animation_state` of title `title` at `revision`
/// (Protocol 2.29), which follows a snapshot's other follow-ons.
fn animation_state(revision: u64, title: u128) -> fm_protocol::TitleAnimationStateMessage {
    fm_protocol::TitleAnimationStateMessage {
        revision,
        input: input(title),
        animation_revision: 0,
        clip: Box::new(
            fm_protocol::WireTitleClip::new(
                fm_animation::Timebase::new(fm_types::FrameRate::new(30, 1).unwrap()),
                0,
                0,
            )
            .unwrap(),
        ),
    }
}

/// The `title_layout_state` of title `title` at `revision` (Protocol 2.30):
/// a 1920x1080 canvas with one named text element. It follows the
/// animation states and completes the snapshot.
fn layout_state(revision: u64, title: u128) -> fm_protocol::TitleLayoutStateMessage {
    fm_protocol::TitleLayoutStateMessage {
        revision,
        input: input(title),
        layout: Box::new(fm_protocol::WireTitleLayout {
            width: 1_920,
            height: 1_080,
            elements: vec![fm_protocol::WireTitleElementOutline {
                id: fm_protocol::WireTitleElementId::new(core::num::NonZeroU128::new(1).unwrap()),
                name: "Name".into(),
                kind: fm_protocol::WireTitleElementKind::Text,
                x: 160,
                y: 860,
                width: 700,
                height: 60,
                opacity: 255,
                fill: fm_animation::Rgba::new(255, 255, 255, 255),
                visible: true,
            }],
        }),
    }
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
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
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
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        system_audio_inputs: Vec::new(),
        screen_capture_inputs: Vec::new(),
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
        scenes: Vec::new(),
        layout_scenes: Vec::new(),
        frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
        paged_states: 0,
        cue_list_states: 0,
        script_states: 0,
        shortcut_states: 0,
        still_states: 0,
        media_list_states: 0,
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
        alert_rules: Vec::new(),
        transitions_in_flight: Vec::new(),
    }
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
fn a_typed_refusal_reason_survives_the_wire_into_the_model() {
    // Protocol 2.45: the client keeps the refusal's typed reason, so a
    // console can word it in its own language.
    let mut client = ready_client(8);
    let command = client
        .queue_command(
            CommandPayload::RemoveInput { input: input(2) },
            "remove-2".to_owned(),
            Some(4),
            None,
        )
        .unwrap();
    assert!(matches!(
        client.pop_outbound(),
        Some(Outbound::Command(queued)) if queued == command
    ));
    let reason = fm_protocol::RefusalReason::InputInUse {
        input: 2,
        usage: fm_protocol::InputUse::SceneLayer { scene: 100 },
    };
    let line = fm_protocol::encode_line(&WireMessage::CommandResult(CommandResult::Rejected {
        id: command.id.clone(),
        code: "conflict".to_owned(),
        message: "input 2 cannot be removed: it is a layer source of scene 100".to_owned(),
        fields: Vec::new(),
        current_revision: 4,
        retryable: false,
        reason: Some(reason.clone()),
    }))
    .unwrap();
    let decoded = fm_protocol::decode_line(&line).unwrap();
    assert_eq!(client.intake(decoded).unwrap(), Intake::ResultReconciled);
    let rejection = client.model().last_rejection().unwrap();
    assert_eq!(rejection.code, "conflict");
    assert_eq!(rejection.reason, Some(reason));
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

/// Protocol 2.48 (SW-004): a client reduces a manual Stinger with its clip
/// length and every output's own Fade-to-Black duration and the curve,
/// and queues the timing edits exactly, without optimistic state.
#[test]
fn manual_stingers_and_fade_to_black_timing_reduce_into_the_client_model() {
    let output = fm_protocol::WireOutputId::new(NonZeroU128::new(7).unwrap());
    let stinger = ManualTransitionKind::Stinger {
        slot: WireStingerSlotId::new(2).unwrap(),
        duration_frames: 24,
    };
    let mut client = Client::new(config(4)).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client.accept_handshake(handshake(4, None)).unwrap();
    let mut initial = snapshot(4);
    initial.outputs = vec![fm_protocol::OutputStatus {
        output,
        name: "Program".into(),
    }];
    initial.fade_to_black_output_durations = vec![(output, 90)];
    initial.desired_manual_transition = ManualTransitionStatus::Active(ManualTransitionState {
        kind: stinger,
        from: input(1),
        to: input(2),
        interval_start: ManualTransitionPosition::START,
        position: ManualTransitionPosition::new(5_000).unwrap(),
    });
    client.apply_snapshot(initial).unwrap();
    let state = client.model().state().unwrap();
    assert!(matches!(
        state.switcher().desired_manual_transition,
        ModelManualTransitionStatus::Active(active) if active.kind == stinger
    ));
    assert_eq!(
        state.fade_to_black_output_durations(),
        [(output.to_domain(), 90)]
    );

    for (revision, payload) in [
        (
            5,
            EventPayload::FadeToBlackOutputDurationChanged {
                output,
                duration_frames: Some(45),
            },
        ),
        (
            6,
            EventPayload::FadeToBlackCurveChanged {
                curve: fm_protocol::FadeToBlackCurve::Ease,
            },
        ),
    ] {
        client
            .apply_event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision,
                },
                payload,
            })
            .unwrap();
    }
    let state = client.model().state().unwrap();
    assert_eq!(
        state.fade_to_black_output_durations(),
        [(output.to_domain(), 45)]
    );
    assert_eq!(
        state.fade_to_black_curve(),
        fm_protocol::FadeToBlackCurve::Ease
    );

    let payload = CommandPayload::SetFadeToBlackOutputDuration {
        output,
        duration_frames: None,
    };
    let command = client
        .queue_command(payload.clone(), "ftb-duration", Some(6), None)
        .unwrap();
    assert_eq!(command.payload, payload);
    assert!(matches!(client.pop_outbound(), Some(Outbound::Command(queued)) if queued == command));
    // Nothing changes until the authority's event arrives.
    assert_eq!(
        client
            .model()
            .state()
            .unwrap()
            .fade_to_black_output_durations(),
        [(output.to_domain(), 45)]
    );
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
                    network_stream: None,
                    system_audio: None,
                    screen_capture: None,
                    capture: None,
                    still: None,
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
        lifecycle: fm_protocol::InputLifecycleStatus::default(),
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
            phase: fm_protocol::TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            timeline: None,
            fields: vec![TitleFieldStatus {
                field: field(1),
                name: "Name".into(),
                value: TitleFieldValue::Text("Ana".into()),
                exposed: true,
                revision: 0,
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
        // Protocol 2.29: the title's animation state follows, then (2.30)
        // its layout state completes the snapshot.
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        assert_eq!(
            client
                .intake(WireMessage::TitleAnimationState(animation_state(4, 2)))
                .unwrap(),
            Intake::TitleAnimationStateApplied
        );
        assert!(client.model().state().unwrap().title_animations_complete());
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        assert_eq!(
            client
                .intake(WireMessage::TitleLayoutState(layout_state(4, 2)))
                .unwrap(),
            Intake::TitleLayoutStateApplied
        );
        assert_eq!(client.state(), &ConnectionState::Ready);
        assert!(client.model().state().unwrap().title_layouts_complete());
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
    fn animation_states_out_of_order_or_unannounced_force_a_fresh_snapshot() {
        let resync = |client: &mut Client, revision: u64, title: u128| {
            let error = client
                .intake(WireMessage::TitleAnimationState(animation_state(
                    revision, title,
                )))
                .unwrap_err();
            assert!(
                matches!(error, ClientError::ResyncRequired { .. }),
                "{error:?}"
            );
        };
        // Before the title's own state.
        resync(&mut synchronizing_client(), 4, 2);
        let completed_titles = || {
            let mut client = synchronizing_client();
            client
                .intake(WireMessage::TitleState(title_state(4, 3)))
                .unwrap();
            client
        };
        // At another revision, or for a title the snapshot did not announce.
        resync(&mut completed_titles(), 3, 2);
        resync(&mut completed_titles(), 4, 1);
        // A second one for the same title.
        let mut client = completed_titles();
        client
            .intake(WireMessage::TitleAnimationState(animation_state(4, 2)))
            .unwrap();
        resync(&mut client, 4, 2);
    }

    #[test]
    fn layout_states_out_of_order_or_unannounced_force_a_fresh_snapshot() {
        let resync = |client: &mut Client, revision: u64, title: u128| {
            let error = client
                .intake(WireMessage::TitleLayoutState(layout_state(revision, title)))
                .unwrap_err();
            assert!(
                matches!(error, ClientError::ResyncRequired { .. }),
                "{error:?}"
            );
        };
        let completed_titles = || {
            let mut client = synchronizing_client();
            client
                .intake(WireMessage::TitleState(title_state(4, 3)))
                .unwrap();
            client
        };
        // Before the title states, and before the animation states.
        resync(&mut synchronizing_client(), 4, 2);
        resync(&mut completed_titles(), 4, 2);
        let animated = || {
            let mut client = completed_titles();
            client
                .intake(WireMessage::TitleAnimationState(animation_state(4, 2)))
                .unwrap();
            client
        };
        // At another revision, or for a title the snapshot did not announce.
        resync(&mut animated(), 3, 2);
        resync(&mut animated(), 4, 1);
        // In order it completes the snapshot with the layout; a second one
        // after that is out of order.
        let mut client = animated();
        client
            .intake(WireMessage::TitleLayoutState(layout_state(4, 2)))
            .unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);
        let layout = client
            .model()
            .state()
            .unwrap()
            .title(input(2).to_domain())
            .unwrap()
            .layout
            .clone()
            .unwrap();
        assert_eq!(*layout, *layout_state(4, 2).layout);
        resync(&mut client, 4, 2);
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
        client
            .intake(WireMessage::TitleAnimationState(animation_state(4, 2)))
            .unwrap();
        client
            .intake(WireMessage::TitleLayoutState(layout_state(4, 2)))
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

/// Protocol 2.26: a snapshot announces its playlist and one `playlist_state`
/// completes it after the title states.
mod playlists {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistRunState, PlaylistStateMessage,
        PlaylistTransitionKind, TitleFieldStatus, TitleFieldValue, TitleStateMessage, TitleSummary,
        WireTitleFieldId,
    };

    use super::*;

    fn announced_snapshot(revision: u64, titles: bool) -> SnapshotMessage {
        SnapshotMessage {
            titles: if titles {
                vec![TitleSummary {
                    input: input(2),
                    field_revision: 0,
                }]
            } else {
                Vec::new()
            },
            playlist_follows: true,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            paged_states: 0,
            cue_list_states: 0,
            script_states: 0,
            shortcut_states: 0,
            still_states: 0,
            media_list_states: 0,
            input_categories: Vec::new(),
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
            ..snapshot(revision)
        }
    }

    fn title_state(revision: u64) -> TitleStateMessage {
        TitleStateMessage {
            revision,
            input: input(2),
            name: "Lower third".into(),
            field_revision: 0,
            phase: fm_protocol::TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            timeline: None,
            fields: vec![TitleFieldStatus {
                field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
                name: "Name".into(),
                value: TitleFieldValue::Text("Ana".into()),
                exposed: true,
                revision: 0,
            }],
        }
    }

    fn playlist_state(revision: u64, entry_input: u128) -> PlaylistStateMessage {
        PlaylistStateMessage {
            revision,
            frame: 900,
            loop_mode: PlaylistLoopMode::All,
            entries: vec![PlaylistEntrySpec {
                id: 7,
                input: input(entry_input),
                duration_frames: 300,
                transition: PlaylistTransitionKind::Fade,
                transition_frames: 15,
                enabled: true,
            }],
            state: PlaylistRunState::Playing {
                entry: 7,
                take: 1,
                cycle: 0,
                started_frame: 850,
            },
        }
    }

    fn synchronizing(snapshot: SnapshotMessage) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client.intake(WireMessage::Snapshot(snapshot)).unwrap();
        client
    }

    #[test]
    fn a_snapshot_without_a_playlist_implies_the_empty_one() {
        let client = synchronizing(snapshot(4));
        assert_eq!(client.state(), &ConnectionState::Ready);
        let state = client.model().state().unwrap();
        assert_eq!(state.frame_rate(), fm_types::FrameRate::new(30, 1).unwrap());
        assert_eq!(
            state.playlist(),
            Some(&fm_ui_model::PlaylistStatus::empty())
        );
    }

    #[test]
    fn an_announced_playlist_completes_the_snapshot_after_its_titles() {
        let mut client = synchronizing(announced_snapshot(4, true));
        assert!(client.model().state().unwrap().playlist().is_none());
        client
            .intake(WireMessage::TitleState(title_state(4)))
            .unwrap();
        // Titles alone do not finish a snapshot that announced a playlist.
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        assert_eq!(
            client
                .intake(WireMessage::PlaylistState(playlist_state(4, 1)))
                .unwrap(),
            Intake::PlaylistStateApplied
        );
        client
            .intake(WireMessage::TitleAnimationState(animation_state(4, 2)))
            .unwrap();
        client
            .intake(WireMessage::TitleLayoutState(layout_state(4, 2)))
            .unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);
        let playlist = client.model().state().unwrap().playlist().unwrap().clone();
        assert_eq!(playlist.frame, 900);
        assert_eq!(playlist.loop_mode, PlaylistLoopMode::All);
        assert_eq!(playlist.entries.len(), 1);
        assert!(matches!(
            playlist.state,
            PlaylistRunState::Playing {
                started_frame: 850,
                ..
            }
        ));
    }

    #[test]
    fn out_of_order_or_unannounced_playlist_states_force_a_fresh_snapshot() {
        let resync = |client: &mut Client, message: PlaylistStateMessage| {
            let error = client
                .intake(WireMessage::PlaylistState(message))
                .unwrap_err();
            assert!(
                matches!(error, ClientError::ResyncRequired { .. }),
                "{error:?}"
            );
        };
        // Before the announced title states.
        let mut client = synchronizing(announced_snapshot(4, true));
        resync(&mut client, playlist_state(4, 1));
        // At another revision.
        let mut client = synchronizing(announced_snapshot(4, false));
        resync(&mut client, playlist_state(3, 1));
        // Naming an input the snapshot does not have.
        let mut client = synchronizing(announced_snapshot(4, false));
        resync(&mut client, playlist_state(4, 99));
        // Not announced, so the client is already ready.
        let mut client = synchronizing(snapshot(4));
        resync(&mut client, playlist_state(4, 1));
        // A second one after the first completed the snapshot.
        let mut client = synchronizing(announced_snapshot(4, false));
        client
            .intake(WireMessage::PlaylistState(playlist_state(4, 1)))
            .unwrap();
        resync(&mut client, playlist_state(4, 1));
    }

    #[test]
    fn an_incomplete_playlist_never_resumes_and_events_wait_for_it() {
        let mut client = synchronizing(announced_snapshot(4, false));
        let event = EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision: 5,
            },
            payload: EventPayload::PlaylistChanged {
                frame: 950,
                loop_mode: PlaylistLoopMode::Off,
                entries: Vec::new(),
                state: PlaylistRunState::Stopped,
                program: input(1),
                preview: input(2),
            },
        };
        let error = client.apply_event(event).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
        let mut client = synchronizing(announced_snapshot(4, false));
        let _ = client.transport_disconnected();
        client.start_connect().unwrap();
        assert_eq!(client.transport_connected().unwrap().resume_cursor, None);
    }
}

/// Protocol 2.28: a snapshot announces its `grade_state` records, which
/// complete it after the playlist state, and `grade_changed` keeps the
/// replicated grading current.
mod grading {
    use core::num::NonZeroU32;

    use fm_protocol::{GradeChange, GradeEntry, GradeStateMessage, WireGradeSource};

    use super::*;

    fn id(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }

    fn warm() -> Box<fm_grade::Grade> {
        Box::new(fm_grade::builtin_preset("warm").unwrap().grade.clone())
    }

    fn states(revision: u64) -> Vec<GradeStateMessage> {
        [
            GradeEntry::Lut {
                lut: id(3),
                name: "Film".into(),
                kind: fm_grade::CubeKind::ThreeD,
                size: 17,
            },
            GradeEntry::Library {
                grade: id(1),
                name: "Warm".into(),
                value: warm(),
            },
            GradeEntry::Input {
                input: input(2),
                source: WireGradeSource::Library(id(1)),
            },
            GradeEntry::Program {
                source: WireGradeSource::Inline(warm()),
            },
        ]
        .into_iter()
        .map(|entry| GradeStateMessage { revision, entry })
        .collect()
    }

    fn synchronizing(revision: u64, grade_states: u16) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(revision, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(SnapshotMessage {
                grade_states,
                ..snapshot(revision)
            }))
            .unwrap();
        client
    }

    fn resyncs(client: &mut Client, state: GradeStateMessage) {
        let error = client.intake(WireMessage::GradeState(state)).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn announced_grade_states_complete_the_snapshot_in_order() {
        let mut client = synchronizing(4, 4);
        for state in states(4) {
            assert!(!client.model().state().unwrap().grading_complete());
            assert!(matches!(
                client.state(),
                ConnectionState::Synchronizing {
                    mode: SyncMode::Snapshot,
                    ..
                }
            ));
            assert_eq!(
                client.intake(WireMessage::GradeState(state)).unwrap(),
                Intake::GradeStateApplied
            );
        }
        assert_eq!(client.state(), &ConnectionState::Ready);
        let grading = client.model().state().unwrap().grading().clone();
        assert_eq!(grading.luts.len(), 1);
        assert_eq!(grading.library[0].name, "Warm");
        assert_eq!(
            grading.resolve(&grading.inputs[&input(2).to_domain()]),
            Some(&*warm())
        );
        assert_eq!(grading.program, Some(WireGradeSource::Inline(warm())));

        // grade_changed edits the replicated view; an unknown library grade
        // is refused.
        let event = |revision: u64, change: GradeChange| {
            WireMessage::Event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision,
                },
                payload: EventPayload::GradeChanged(change),
            })
        };
        client
            .intake(event(
                5,
                GradeChange::Input {
                    input: input(2),
                    source: None,
                },
            ))
            .unwrap();
        assert!(client.model().state().unwrap().grading().inputs.is_empty());
        client
            .intake(event(6, GradeChange::Program { source: None }))
            .unwrap();
        assert_eq!(client.model().state().unwrap().grading().program, None);
        assert!(
            client
                .intake(event(
                    7,
                    GradeChange::Input {
                        input: input(1),
                        source: Some(WireGradeSource::Library(id(9))),
                    },
                ))
                .is_err()
        );
    }

    #[test]
    fn unannounced_out_of_order_or_stale_grade_states_force_a_fresh_snapshot() {
        // Beyond the announced count.
        let mut client = synchronizing(4, 0);
        assert_eq!(client.state(), &ConnectionState::Ready);
        resyncs(&mut client, states(4).remove(0));

        // Out of order: an input before the library grade it names.
        let mut client = synchronizing(4, 4);
        resyncs(&mut client, states(4).remove(2));

        // At another revision.
        let mut client = synchronizing(4, 4);
        resyncs(&mut client, states(3).remove(0));

        // Naming an input the snapshot lacks.
        let mut client = synchronizing(4, 1);
        resyncs(
            &mut client,
            GradeStateMessage {
                revision: 4,
                entry: GradeEntry::Input {
                    input: input(9),
                    source: WireGradeSource::Inline(warm()),
                },
            },
        );

        // An event before the announced states arrive.
        let mut client = synchronizing(4, 4);
        let error = client
            .intake(WireMessage::Event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 5,
                },
                payload: EventPayload::GradeChanged(GradeChange::Program { source: None }),
            }))
            .unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    fn scene(value: u128) -> fm_protocol::WireSceneId {
        fm_protocol::WireSceneId::new(core::num::NonZeroU128::new(value).unwrap())
    }

    /// Protocol 2.35: a scene layer grade replicates by scene and layer,
    /// after the input grades; `grade_changed` keeps it current, and a
    /// layer record before an input record, or out of scene then layer
    /// order, is out of contract.
    #[test]
    fn scene_layer_grades_replicate_by_scene() {
        let layer = fm_protocol::WireSceneLayerId::new(id(7));
        let layer_state = |revision: u64| GradeStateMessage {
            revision,
            entry: GradeEntry::Layer {
                scene: scene(5),
                layer,
                source: WireGradeSource::Library(id(1)),
            },
        };
        let mut client = synchronizing(4, 5);
        let mut ordered = states(4);
        ordered.insert(3, layer_state(4));
        for state in ordered {
            assert_eq!(
                client.intake(WireMessage::GradeState(state)).unwrap(),
                Intake::GradeStateApplied
            );
        }
        assert_eq!(client.state(), &ConnectionState::Ready);
        let grading = client.model().state().unwrap().grading().clone();
        assert_eq!(
            grading.layer(scene(5).to_domain(), layer),
            Some(&WireGradeSource::Library(id(1)))
        );
        assert_eq!(grading.layer(scene(6).to_domain(), layer), None);
        client
            .intake(WireMessage::Event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 5,
                },
                payload: EventPayload::GradeChanged(GradeChange::Layer {
                    scene: scene(5),
                    layer,
                    source: None,
                }),
            }))
            .unwrap();
        assert!(client.model().state().unwrap().grading().layers.is_empty());

        // A layer before the input grades, or after a later scene's layer, is
        // out of contract.
        let mut client = synchronizing(4, 5);
        for state in states(4).into_iter().take(2) {
            client.intake(WireMessage::GradeState(state)).unwrap();
        }
        client
            .intake(WireMessage::GradeState(layer_state(4)))
            .unwrap();
        resyncs(&mut client, states(4).remove(2));
        let mut client = synchronizing(4, 2);
        let next_scene = GradeStateMessage {
            revision: 4,
            entry: GradeEntry::Layer {
                scene: scene(6),
                layer: fm_protocol::WireSceneLayerId::new(id(1)),
                source: WireGradeSource::Inline(warm()),
            },
        };
        client.intake(WireMessage::GradeState(next_scene)).unwrap();
        resyncs(&mut client, layer_state(4));
    }
}

/// Protocol 2.31 (GX-011 stage 5): a snapshot's Scene inputs complete with
/// their `scene_animation_state`, then every project scene (Protocol 2.35:
/// shown or not) with its `scene_layout_state`, and
/// `scene_animation_changed` keeps every Scene input showing the scene
/// current.
mod scene_animation {
    use super::*;

    /// Inputs 1 and 2 both show scene 10; scene 11 is shown by none.
    const SHOWN: u128 = 10;
    const SPARE: u128 = 11;

    fn scene(value: u128) -> fm_protocol::WireSceneId {
        fm_protocol::WireSceneId::new(core::num::NonZeroU128::new(value).unwrap())
    }

    fn layer(value: u32) -> fm_protocol::WireSceneLayerId {
        fm_protocol::WireSceneLayerId::new(core::num::NonZeroU32::new(value).unwrap())
    }

    fn clip(in_frames: u32) -> fm_protocol::WireSceneClip {
        fm_protocol::WireSceneClip::new(
            fm_animation::Timebase::new(fm_types::FrameRate::new(30, 1).unwrap()),
            in_frames,
            0,
        )
        .unwrap()
    }

    fn scene_snapshot(revision: u64) -> SnapshotMessage {
        SnapshotMessage {
            scenes: vec![input(1), input(2)],
            layout_scenes: vec![scene(SHOWN), scene(SPARE)],
            ..snapshot(revision)
        }
    }

    fn animation(revision: u64, shown_by: u128) -> fm_protocol::SceneAnimationStateMessage {
        fm_protocol::SceneAnimationStateMessage {
            revision,
            input: input(shown_by),
            scene: scene(SHOWN),
            animation_revision: 0,
            clip: Box::new(clip(0)),
        }
    }

    fn layout(revision: u64, id: u128) -> fm_protocol::SceneLayoutStateMessage {
        fm_protocol::SceneLayoutStateMessage {
            revision,
            scene: scene(id),
            name: format!("Scene {id}"),
            layout: Box::new(fm_protocol::WireSceneLayout {
                width: 1_920,
                height: 1_080,
                layers: vec![fm_protocol::WireSceneLayerOutline {
                    id: layer(1),
                    name: "Cam".into(),
                    x: 0,
                    y: 0,
                    width: 960,
                    height: 540,
                    quarter_turns: 0,
                    opacity: 255,
                    enabled: true,
                    tilt_millidegrees: 0,
                    anchor_x: 5_000,
                    anchor_y: 5_000,
                    z_order: 0,
                    crop: None,
                    key: None,
                    effects: Vec::new(),
                    mask: None,
                    source: fm_protocol::WireSceneLayerSource::Input(input(3)),
                }],
            }),
        }
    }

    fn synchronizing_client() -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(scene_snapshot(4)))
            .unwrap();
        client
    }

    #[test]
    fn scene_states_complete_the_snapshot_and_changes_replicate() {
        let mut client = synchronizing_client();
        for scene in [1, 2] {
            assert_eq!(
                client
                    .intake(WireMessage::SceneAnimationState(animation(4, scene)))
                    .unwrap(),
                Intake::SceneAnimationStateApplied
            );
        }
        assert!(client.model().state().unwrap().scene_animations_complete());
        assert_eq!(
            client
                .intake(WireMessage::SceneLayoutState(layout(4, SHOWN)))
                .unwrap(),
            Intake::SceneLayoutStateApplied
        );
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        client
            .intake(WireMessage::SceneLayoutState(layout(4, SPARE)))
            .unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);
        let state = client.model().state().unwrap();
        assert!(state.scene_layouts_complete());
        // Both Scene inputs share their scene's layout.
        for shown_by in [1, 2] {
            let shown = state.scene(input(shown_by).to_domain()).unwrap();
            assert_eq!(shown.scene, Some(scene(SHOWN).to_domain()));
            assert_eq!(shown.layout.as_ref().unwrap().layers[0].name, "Cam");
        }
        // The unshown scene is replicated by its id and name.
        let spare = state.scene_layout(scene(SPARE).to_domain()).unwrap();
        assert_eq!(spare.name, "Scene 11");
        assert_eq!(
            spare.layout.as_ref().unwrap().layers[0].source,
            fm_protocol::WireSceneLayerSource::Input(input(3))
        );
        assert_eq!(state.scene_layouts().len(), 2);

        client
            .apply_event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 5,
                },
                payload: EventPayload::SceneAnimationChanged {
                    inputs: vec![input(1), input(2)],
                    animation_revision: 1,
                    clip: Box::new(clip(12)),
                },
            })
            .unwrap();
        for scene in client.model().state().unwrap().scenes() {
            let animation = scene.animation.as_ref().unwrap();
            assert_eq!(animation.revision, 1);
            assert_eq!(animation.clip.in_frames(), 12);
        }

        // A change that does not advance the revision is refused.
        let error = client
            .apply_event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 6,
                },
                payload: EventPayload::SceneAnimationChanged {
                    inputs: vec![input(1)],
                    animation_revision: 1,
                    clip: Box::new(clip(6)),
                },
            })
            .unwrap_err();
        assert!(
            matches!(
                error,
                ClientError::Model(fm_ui_model::ModelError::InvalidSceneChange(_))
            ),
            "{error:?}"
        );
    }

    /// SW-006, Protocol 2.39: a live layer edit's `scene_layout_changed`
    /// replaces the scene's replicated layout everywhere it is shown, and
    /// a change for a scene without a replicated layout is refused.
    #[test]
    fn scene_layout_changes_replace_the_replicated_layout() {
        let mut client = synchronizing_client();
        for shown_by in [1, 2] {
            client
                .intake(WireMessage::SceneAnimationState(animation(4, shown_by)))
                .unwrap();
        }
        for id in [SHOWN, SPARE] {
            client
                .intake(WireMessage::SceneLayoutState(layout(4, id)))
                .unwrap();
        }
        assert_eq!(client.state(), &ConnectionState::Ready);
        let mut edited = (*layout(4, SHOWN).layout).clone();
        let geometry = fm_protocol::WireLayerGeometry {
            x: 100,
            y: 60,
            width: 640,
            height: 360,
            quarter_turns: 1,
            tilt_millidegrees: -2_500,
            anchor_x: 0,
            anchor_y: 10_000,
        };
        edited.layers[0].set_geometry(geometry);
        edited.layers[0].z_order = 4;
        let edit = fm_protocol::SceneLayerEdit {
            scene: scene(SHOWN),
            layer: layer(1),
            operation: fm_protocol::SceneLayerOperation::SetGeometry(geometry),
            continues: true,
            gesture: 3,
        };
        let change = |revision: u64, edit: fm_protocol::SceneLayerEdit| EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            payload: EventPayload::SceneLayoutChanged {
                edit,
                layout: Box::new(edited.clone()),
            },
        };
        client.apply_event(change(5, edit.clone())).unwrap();
        let state = client.model().state().unwrap();
        for shown_by in [1, 2] {
            let shown = state.scene(input(shown_by).to_domain()).unwrap();
            assert_eq!(shown.layout.as_deref(), Some(&edited));
        }
        let entry = state.scene_layout(scene(SHOWN).to_domain()).unwrap();
        assert_eq!(entry.layout.as_deref(), Some(&edited));
        assert_eq!(entry.name, "Scene 10");
        let spare = state.scene_layout(scene(SPARE).to_domain()).unwrap();
        assert_eq!(spare.layout.as_deref(), Some(&*layout(4, SPARE).layout));

        // A scene the client has no layout for is a mismatch.
        let error = client
            .apply_event(change(
                6,
                fm_protocol::SceneLayerEdit {
                    scene: scene(12),
                    ..edit
                },
            ))
            .unwrap_err();
        assert!(
            matches!(
                error,
                ClientError::Model(fm_ui_model::ModelError::SceneLayoutMismatch(_))
            ),
            "{error:?}"
        );
    }

    #[test]
    fn out_of_order_scene_states_force_a_fresh_snapshot() {
        // A layout before every animation.
        let mut client = synchronizing_client();
        let error = client
            .intake(WireMessage::SceneLayoutState(layout(4, SHOWN)))
            .unwrap_err();
        assert!(matches!(error, ClientError::ResyncRequired { .. }));
        let mut client = synchronizing_client();
        client
            .intake(WireMessage::SceneAnimationState(animation(4, 1)))
            .unwrap();
        let error = client
            .intake(WireMessage::SceneLayoutState(layout(4, SPARE)))
            .unwrap_err();
        assert!(matches!(error, ClientError::ResyncRequired { .. }));

        // A layout for a scene the snapshot did not announce, or twice.
        for unannounced in [12, SHOWN] {
            let mut client = synchronizing_client();
            for shown_by in [1, 2] {
                client
                    .intake(WireMessage::SceneAnimationState(animation(4, shown_by)))
                    .unwrap();
            }
            if unannounced == SHOWN {
                client
                    .intake(WireMessage::SceneLayoutState(layout(4, SHOWN)))
                    .unwrap();
            }
            let error = client
                .intake(WireMessage::SceneLayoutState(layout(4, unannounced)))
                .unwrap_err();
            assert!(matches!(error, ClientError::ResyncRequired { .. }));
        }

        // A scene the snapshot did not announce.
        let mut client = synchronizing_client();
        let error = client
            .intake(WireMessage::SceneAnimationState(animation(4, 3)))
            .unwrap_err();
        assert!(matches!(error, ClientError::ResyncRequired { .. }));

        // A record at another revision.
        let mut client = synchronizing_client();
        let error = client
            .intake(WireMessage::SceneAnimationState(animation(3, 1)))
            .unwrap_err();
        assert!(matches!(error, ClientError::ResyncRequired { .. }));

        // A scene state once ready.
        let mut client = ready_client(4);
        let error = client
            .intake(WireMessage::SceneAnimationState(animation(4, 1)))
            .unwrap_err();
        assert!(matches!(error, ClientError::ResyncRequired { .. }));
    }
}

/// Protocol 2.34: a snapshot announces its `animation_preset_state`
/// records, which complete it after every other follow-on record, and
/// `animation_preset_changed` keeps the replicated library current.
mod animation_presets {
    use core::num::NonZeroU32;

    use fm_animation::{
        Easing, Keyframe, MotionClip, MotionTarget, Phase, PhaseMotion, Property, Timebase, Track,
        TrackKey, Value,
    };
    use fm_protocol::{
        AnimationPresetChange, AnimationPresetEntry, AnimationPresetStateMessage, GradeEntry,
        GradeStateMessage, WireGradeSource,
    };

    use super::*;

    fn entry(preset: u32, name: &str) -> AnimationPresetEntry {
        let clip = MotionClip::from_tracks(
            Timebase::new(fm_types::FrameRate::new(25, 1).unwrap()),
            10,
            0,
            vec![Track::new(
                TrackKey::new(MotionTarget, Phase::In, Property::PositionY),
                vec![
                    Keyframe::new(0, Value::Scalar(-50), Easing::CubicOut),
                    Keyframe::new(10, Value::Scalar(0), Easing::Linear),
                ],
            )],
        )
        .unwrap();
        AnimationPresetEntry {
            preset: NonZeroU32::new(preset).unwrap(),
            name: name.into(),
            phase: Phase::In,
            motion: Box::new(PhaseMotion::new(clip, 200, 50).unwrap()),
        }
    }

    fn preset_state(revision: u64, preset: u32, name: &str) -> WireMessage {
        WireMessage::AnimationPresetState(AnimationPresetStateMessage {
            revision,
            entry: entry(preset, name),
        })
    }

    fn grade_state(revision: u64) -> WireMessage {
        WireMessage::GradeState(GradeStateMessage {
            revision,
            entry: GradeEntry::Program {
                source: WireGradeSource::Inline(Box::new(fm_grade::Grade::IDENTITY)),
            },
        })
    }

    fn synchronizing(grade_states: u16, animation_preset_states: u16) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(SnapshotMessage {
                grade_states,
                animation_preset_states,
                ..snapshot(4)
            }))
            .unwrap();
        client
    }

    fn resyncs(client: &mut Client, message: WireMessage) {
        let error = client.intake(message).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn announced_preset_states_complete_the_snapshot_last() {
        let mut client = synchronizing(1, 2);
        // A preset before the grade states it follows is out of order.
        resyncs(&mut client, preset_state(4, 1, "Rise"));

        let mut client = synchronizing(1, 2);
        client.intake(grade_state(4)).unwrap();
        for (preset, name) in [(3, "Rise"), (1, "Drop")] {
            assert!(matches!(
                client.state(),
                ConnectionState::Synchronizing {
                    mode: SyncMode::Snapshot,
                    ..
                }
            ));
            assert_eq!(
                client.intake(preset_state(4, preset, name)).unwrap(),
                Intake::AnimationPresetStateApplied
            );
        }
        assert_eq!(client.state(), &ConnectionState::Ready);
        let library = client.model().state().unwrap().animation_presets().clone();
        assert_eq!(
            library
                .entries()
                .iter()
                .map(|entry| (entry.preset.get(), entry.name.as_str()))
                .collect::<Vec<_>>(),
            [(3, "Rise"), (1, "Drop")]
        );
        assert_eq!(*library.entries()[0].motion, *entry(3, "Rise").motion);

        // animation_preset_changed edits the replicated library; a removal
        // of an unknown preset is refused.
        let event = |revision: u64, change: AnimationPresetChange| {
            WireMessage::Event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision,
                },
                payload: EventPayload::AnimationPresetChanged(change),
            })
        };
        client
            .intake(event(
                5,
                AnimationPresetChange::Set {
                    index: 1,
                    entry: entry(1, "Fall"),
                },
            ))
            .unwrap();
        client
            .intake(event(
                6,
                AnimationPresetChange::Removed {
                    preset: NonZeroU32::new(3).unwrap(),
                },
            ))
            .unwrap();
        let library = client.model().state().unwrap().animation_presets().clone();
        assert_eq!(library.entries().len(), 1);
        assert_eq!(library.entries()[0].name, "Fall");
        assert!(
            client
                .intake(event(
                    7,
                    AnimationPresetChange::Removed {
                        preset: NonZeroU32::new(3).unwrap(),
                    },
                ))
                .is_err()
        );
    }

    #[test]
    fn unannounced_stale_or_duplicate_preset_states_force_a_fresh_snapshot() {
        // Beyond the announced count.
        let mut client = synchronizing(0, 0);
        assert_eq!(client.state(), &ConnectionState::Ready);
        resyncs(&mut client, preset_state(4, 1, "Rise"));

        // At another revision.
        let mut client = synchronizing(0, 1);
        resyncs(&mut client, preset_state(3, 1, "Rise"));

        // A repeated name.
        let mut client = synchronizing(0, 2);
        client.intake(preset_state(4, 1, "Rise")).unwrap();
        resyncs(&mut client, preset_state(4, 2, "Rise"));

        // An event before the announced states arrive.
        let mut client = synchronizing(0, 1);
        resyncs(
            &mut client,
            WireMessage::Event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 5,
                },
                payload: EventPayload::AnimationPresetChanged(AnimationPresetChange::Removed {
                    preset: NonZeroU32::new(1).unwrap(),
                }),
            }),
        );
    }
}

/// Protocol 2.37 (IN-003): a snapshot announces one `media_state` per media
/// input, which complete it after every other follow-on record, and `media`
/// runtime events keep each input's transport current.
mod media {
    use fm_protocol::{
        MediaEvent, MediaSettingsStatus, MediaStateMessage, MediaStatus, MediaTime,
        MediaTransportState, MediaTransportStatus,
    };

    use super::*;

    fn status(input_id: u128, state: MediaTransportState, position: u64) -> MediaStatus {
        MediaStatus {
            input: input(input_id),
            settings: MediaSettingsStatus::default(),
            transport: MediaTransportStatus {
                available: true,
                state,
                position,
                position_time: MediaTime {
                    numerator: u128::from(position),
                    denominator: 25,
                }
                .reduced(),
                playable_end: 250,
                frames: 250,
                complete: true,
                loops: 0,
                audio_track: None,
                tracks: Vec::new(),
                source_rate: Some(fm_types::FrameRate::new(25, 1).unwrap()),
                fault: None,
            },
        }
    }

    fn media_state(revision: u64, input_id: u128) -> WireMessage {
        WireMessage::MediaState(MediaStateMessage {
            revision,
            frame: 7,
            status: status(input_id, MediaTransportState::Stopped, 0),
        })
    }

    fn synchronizing(media_states: u16) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(SnapshotMessage {
                media_states,
                ..snapshot(4)
            }))
            .unwrap();
        client
    }

    fn resyncs(client: &mut Client, message: WireMessage) {
        let error = client.intake(message).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn announced_media_states_complete_the_snapshot_and_runtime_events_track_them() {
        let mut client = synchronizing(2);
        assert_eq!(
            client.intake(media_state(4, 1)).unwrap(),
            Intake::MediaStateApplied
        );
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        client.intake(media_state(4, 2)).unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);
        let media = client.model().state().unwrap().media().clone();
        assert_eq!(media.len(), 2);

        client
            .apply_runtime_event(runtime_event(
                4,
                9,
                1,
                RuntimeLifecycleEvent::Media {
                    frame: 30,
                    events: vec![MediaEvent::Resumed { frame: 0 }],
                    status: Box::new(status(2, MediaTransportState::Playing, 23)),
                },
            ))
            .unwrap();
        let replicated = client
            .model()
            .state()
            .unwrap()
            .media_status(input(2).to_domain())
            .unwrap()
            .clone();
        assert_eq!(replicated.transport.state, MediaTransportState::Playing);
        assert_eq!(replicated.transport.position, 23);
        assert_eq!(
            replicated.transport.position_time,
            MediaTime {
                numerator: 23,
                denominator: 25
            }
        );
        // The runtime event does not move the durable cursor.
        assert_eq!(client.last_applied_cursor().unwrap().revision, 4);
    }

    #[test]
    fn unannounced_mismatched_or_unknown_media_states_force_a_snapshot() {
        // Beyond the announced count.
        let mut client = synchronizing(1);
        client.intake(media_state(4, 1)).unwrap();
        resyncs(&mut client, media_state(4, 2));
        // At another revision.
        resyncs(&mut synchronizing(1), media_state(3, 1));
        // An input the snapshot does not list.
        resyncs(&mut synchronizing(1), media_state(4, 9));
        // Once ready.
        resyncs(&mut ready_client(4), media_state(4, 1));
        // A runtime status for an input the show lacks.
        let mut client = ready_client(4);
        assert!(
            client
                .apply_runtime_event(runtime_event(
                    4,
                    9,
                    1,
                    RuntimeLifecycleEvent::Media {
                        frame: 1,
                        events: Vec::new(),
                        status: Box::new(status(9, MediaTransportState::Playing, 1)),
                    },
                ))
                .is_err()
        );
    }
}

/// Protocol 2.50 (RC-017): a snapshot announces one `cue_list_state` per
/// cue list, which complete it after its media states; `cue_list_changed`,
/// `cue_list_removed`, and `cue_run_changed` keep the lists and the switcher
/// current.
mod cue_lists {
    use fm_protocol::{
        CueListSpec, CueListStateMessage, CueListStatus, CueRunChange, CueRunStatus,
        PlaylistRunState, TransitionFavoriteEffect, WireCue, WireCueAction,
    };

    use super::*;

    fn list(id: u64, input_id: u128) -> CueListStatus {
        CueListStatus {
            id,
            spec: CueListSpec {
                name: format!("List {id}"),
                cues: vec![WireCue {
                    id: 10,
                    name: "Open".into(),
                    auto_follow: false,
                    actions: vec![WireCueAction::Take {
                        input: Some(fm_types::InputTarget::Fixed(input(input_id))),
                        effect: TransitionFavoriteEffect::Cut,
                        duration_frames: 0,
                    }],
                }],
            },
            state: CueRunStatus {
                standby: Some(10),
                ..CueRunStatus::default()
            },
        }
    }

    fn cue_list_state(revision: u64, id: u64, input_id: u128) -> WireMessage {
        WireMessage::CueListState(CueListStateMessage {
            revision,
            frame: 7,
            list: list(id, input_id),
        })
    }

    fn synchronizing(cue_list_states: u16, media_states: u16) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(SnapshotMessage {
                cue_list_states,
                media_states,
                ..snapshot(4)
            }))
            .unwrap();
        client
    }

    fn resyncs(client: &mut Client, message: WireMessage) {
        let error = client.intake(message).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    fn event(revision: u64, payload: EventPayload) -> EventMessage {
        EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            payload,
        }
    }

    #[test]
    fn announced_cue_list_states_complete_the_snapshot_and_events_track_them() {
        let mut client = synchronizing(2, 0);
        assert_eq!(
            client.intake(cue_list_state(4, 1, 2)).unwrap(),
            Intake::CueListStateApplied
        );
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        client.intake(cue_list_state(4, 3, 1)).unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);
        let state = client.model().state().unwrap();
        assert_eq!(
            state
                .cue_lists()
                .iter()
                .map(|list| list.id)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );

        // GO on list 1 cuts input 2 to Program and plays cue 10.
        let played = CueRunStatus {
            standby: None,
            live: Some(10),
            pending: None,
            played: vec![10],
            fault: None,
            armed: None,
            targets: None,
        };
        client
            .apply_event(event(
                5,
                EventPayload::CueRunChanged(Box::new(CueRunChange {
                    list: 1,
                    frame: 9,
                    state: played.clone(),
                    fired: vec![10],
                    program: input(2),
                    preview: input(1),
                    manual_transition: ManualTransitionStatus::Inactive,
                    fade_to_black: live_fade_to_black(),
                    overlays: fm_protocol::OverlayStatus::empty_channels(),
                    input_audio_strips: input_audio_strips(),
                    streams: Vec::new(),
                    recordings: recordings(),
                    playlist: PlaylistRunState::Stopped,
                    titles: Vec::new(),
                    media: Vec::new(),
                    scripts: Vec::new(),
                })),
            ))
            .unwrap();
        let state = client.model().state().unwrap();
        assert_eq!(state.cue_list(1).unwrap().state, played);
        let view = client.model().view().unwrap();
        assert_eq!(view.switcher.desired.program, input(2).to_domain());

        // A list edit replaces the list; a removal drops it.
        let mut edited = list(3, 2);
        edited.spec.name = "Renamed".into();
        client
            .apply_event(event(
                6,
                EventPayload::CueListChanged(Box::new(edited.clone())),
            ))
            .unwrap();
        assert_eq!(client.model().state().unwrap().cue_list(3), Some(&edited));
        client
            .apply_event(event(7, EventPayload::CueListRemoved { list: 1 }))
            .unwrap();
        assert!(client.model().state().unwrap().cue_list(1).is_none());
    }

    /// Protocol 2.61: each script step a `cue_run_changed` names becomes
    /// that script's last run; naming a script the project lacks forces a
    /// snapshot.
    #[test]
    fn a_cue_run_records_the_scripts_it_ran() {
        use fm_protocol::{CueScriptRun, ScriptSpec, ScriptStatus};

        let run = |scripts| {
            EventPayload::CueRunChanged(Box::new(CueRunChange {
                list: 1,
                frame: 9,
                state: CueRunStatus {
                    live: Some(10),
                    played: vec![10],
                    ..CueRunStatus::default()
                },
                fired: vec![10],
                program: input(2),
                preview: input(1),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: live_fade_to_black(),
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
                streams: Vec::new(),
                recordings: recordings(),
                playlist: PlaylistRunState::Stopped,
                titles: Vec::new(),
                media: Vec::new(),
                scripts,
            }))
        };
        let ran = |script| CueScriptRun {
            script,
            actions: 2,
            instructions: 5,
        };
        let prepared = || {
            let mut client = ready_client(4);
            client
                .apply_event(event(
                    5,
                    EventPayload::ScriptChanged(Box::new(ScriptStatus {
                        id: 3,
                        spec: ScriptSpec {
                            name: "Opening".into(),
                            source: "preview 2\ncut".into(),
                        },
                        source_hidden: false,
                    })),
                ))
                .unwrap();
            let mut runs = list(1, 2);
            runs.spec.cues[0]
                .actions
                .push(WireCueAction::RunScript { script: 3 });
            client
                .apply_event(event(6, EventPayload::CueListChanged(Box::new(runs))))
                .unwrap();
            client
        };

        let mut client = prepared();
        client.apply_event(event(7, run(vec![ran(3)]))).unwrap();
        assert_eq!(
            client.model().state().unwrap().script_run(3),
            Some(fm_ui_model::ScriptRunReport {
                script: 3,
                frame: 9,
                actions: 2,
                instructions: 5,
                cue_list: Some(1),
            })
        );

        let mut client = prepared();
        assert!(client.apply_event(event(7, run(vec![ran(9)]))).is_err());
    }

    #[test]
    fn unannounced_early_or_invalid_cue_list_states_force_a_snapshot() {
        // Beyond the announced count.
        let mut client = synchronizing(1, 0);
        client.intake(cue_list_state(4, 1, 2)).unwrap();
        resyncs(&mut client, cue_list_state(4, 2, 2));
        // At another revision.
        resyncs(&mut synchronizing(1, 0), cue_list_state(3, 1, 2));
        // Naming an input the snapshot does not list.
        resyncs(&mut synchronizing(1, 0), cue_list_state(4, 1, 9));
        // Before the announced media states.
        resyncs(&mut synchronizing(1, 1), cue_list_state(4, 1, 2));
        // Once ready.
        resyncs(&mut ready_client(4), cue_list_state(4, 1, 2));
        // A run change for a list the show lacks.
        let mut client = ready_client(4);
        assert!(
            client
                .apply_event(event(
                    5,
                    EventPayload::CueRunChanged(Box::new(CueRunChange {
                        list: 9,
                        frame: 9,
                        state: CueRunStatus::default(),
                        fired: Vec::new(),
                        program: input(1),
                        preview: input(2),
                        manual_transition: ManualTransitionStatus::Inactive,
                        fade_to_black: live_fade_to_black(),
                        overlays: fm_protocol::OverlayStatus::empty_channels(),
                        input_audio_strips: input_audio_strips(),
                        streams: Vec::new(),
                        recordings: recordings(),
                        playlist: PlaylistRunState::Stopped,
                        titles: Vec::new(),
                        media: Vec::new(),
                        scripts: Vec::new(),
                    })),
                ))
                .is_err()
        );
    }
}

/// OR-011, Protocol 2.42: slot edits replicate through the durable event,
/// runtime status and inventory events update the view, and a
/// `display_inventory` reply is applied to the model.
#[test]
#[allow(clippy::too_many_lines)]
fn display_output_slots_status_and_inventory_replicate() {
    use fm_protocol::{
        DisplayId, DisplayInventory, DisplayInventoryEntry, DisplayInventoryMessage, DisplayMode,
        DisplayOutputSlot, DisplayOutputStatus, HdrCapability, HdrState, WaitReason,
        WireActiveDisplay, WireDisplayOutputState,
    };
    let mut client = ready_client(4);
    let slot = DisplayOutputSlot::new(2).unwrap();
    let config: fm_protocol::DisplayOutputConfig = "display=name:Wall,source=preview,scaling=fill"
        .parse()
        .unwrap();
    let changed = |revision, config: Option<fm_protocol::DisplayOutputConfig>| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            payload: EventPayload::DisplayOutputChanged {
                slot,
                config: config.map(Box::new),
            },
        })
    };
    assert_eq!(
        client.intake(changed(5, Some(config.clone()))).unwrap(),
        Intake::EventApplied
    );
    let view = client.model().view().unwrap();
    assert_eq!(view.display_output(slot), Some(&config));
    // A configured slot waits until the server reports it.
    assert_eq!(
        view.display_output_state(slot),
        Some(&WireDisplayOutputState::Waiting(WaitReason::NoPresenter))
    );

    let waiting = WireDisplayOutputState::Waiting(WaitReason::DisplayAbsent);
    client
        .apply_runtime_event(runtime_event(
            5,
            7,
            1,
            RuntimeLifecycleEvent::DisplayOutputStatus(DisplayOutputStatus {
                slot,
                state: waiting.clone(),
            }),
        ))
        .unwrap();
    assert_eq!(
        client.model().view().unwrap().display_output_state(slot),
        Some(&waiting)
    );
    let active = WireDisplayOutputState::Active(WireActiveDisplay {
        id: DisplayId::new("edid:wall").unwrap(),
        name: "Wall".into(),
        mode: DisplayMode::new(1280, 720, 50_000),
        mode_fallback: false,
        hdr: HdrState::Sdr,
    });
    client
        .apply_runtime_event(runtime_event(
            5,
            7,
            2,
            RuntimeLifecycleEvent::DisplayOutputStatus(DisplayOutputStatus {
                slot,
                state: active.clone(),
            }),
        ))
        .unwrap();
    assert_eq!(
        client.model().view().unwrap().display_output_state(slot),
        Some(&active)
    );

    let inventory = DisplayInventory {
        presenter: true,
        error: None,
        displays: vec![DisplayInventoryEntry {
            id: DisplayId::new("edid:wall").unwrap(),
            name: "Wall".into(),
            x: 0,
            y: 0,
            current_mode: DisplayMode::new(1280, 720, 50_000),
            modes: vec![DisplayMode::new(1280, 720, 50_000)],
            hdr: HdrCapability::Sdr,
        }],
    };
    assert_eq!(client.model().view().unwrap().display_inventory, None);
    client
        .apply_runtime_event(runtime_event(
            5,
            7,
            3,
            RuntimeLifecycleEvent::DisplayInventory(inventory.clone()),
        ))
        .unwrap();
    assert_eq!(
        client.model().view().unwrap().display_inventory.as_ref(),
        Some(&inventory)
    );
    let unplugged = DisplayInventory {
        displays: Vec::new(),
        ..inventory
    };
    assert_eq!(
        client
            .intake(WireMessage::DisplayInventory(DisplayInventoryMessage {
                request_id: "q".into(),
                inventory: unplugged.clone(),
            }))
            .unwrap(),
        Intake::DisplayInventoryApplied
    );
    assert_eq!(
        client.model().view().unwrap().display_inventory.as_ref(),
        Some(&unplugged)
    );
    // A client never receives the request.
    assert!(
        client
            .intake(WireMessage::DisplayInventoryRequest(
                fm_protocol::DisplayInventoryRequest {
                    request_id: "q".into(),
                }
            ))
            .is_err()
    );

    // Clearing the slot removes it and its state.
    client.intake(changed(6, None)).unwrap();
    let view = client.model().view().unwrap();
    assert_eq!(view.display_output(slot), None);
    assert_eq!(view.display_outputs, Vec::new());
}

/// IN-015, Protocol 2.46: a network stream input added live carries its
/// redacted configuration into the view, `network_stream_changed` replaces
/// it (Protocol 2.49: here with an SRT listener's mode, latency, stream ID,
/// audio pair, and masked passphrase), and `network_stream_status` runtime
/// events update its status (with the listening state and the source's
/// channel count).
#[test]
fn network_stream_inputs_and_status_replicate() {
    use fm_protocol::{NetworkStreamConfig, NetworkStreamState, NetworkStreamStatus};
    let mut client = ready_client(4);
    let event = |revision, payload| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            payload,
        })
    };
    let config = NetworkStreamConfig::new("rtsp://***@cam.test/live");
    assert_eq!(
        client
            .intake(event(
                5,
                EventPayload::InputAdded {
                    input: input(5),
                    name: "Remote camera".into(),
                    network_stream: Some(Box::new(config.clone())),
                    system_audio: None,
                    screen_capture: None,
                    capture: None,
                    still: None,
                },
            ))
            .unwrap(),
        Intake::EventApplied
    );
    let view = client.model().view().unwrap();
    let entry = view
        .network_stream(input(5).to_domain())
        .expect("the added stream");
    assert_eq!(entry.config, config);
    assert_eq!(entry.status, NetworkStreamStatus::default());
    assert!(view.network_stream(input(1).to_domain()).is_none());

    let mut edited = NetworkStreamConfig::new("srt://0.0.0.0:9000");
    edited.settings.jitter_buffer_ms = 1_200;
    edited.settings.mode = fm_protocol::StreamMode::Listener;
    edited.settings.latency_ms = 300;
    edited.settings.key_length = fm_protocol::SrtKeyLength::Aes256;
    edited.settings.audio = fm_protocol::AudioChannels::Pair56;
    edited.stream_id = Some("cam1".into());
    edited.passphrase = Some("***".into());
    client
        .intake(event(
            6,
            EventPayload::NetworkStreamChanged {
                input: input(5),
                config: Box::new(edited.clone()),
            },
        ))
        .unwrap();
    let status = NetworkStreamStatus {
        state: NetworkStreamState::Live,
        bitrate_bps: 3_000_000,
        buffer_ms: 1_160,
        dropped_frames: 1,
        late_frames: 2,
        reconnects: 0,
        source_channels: 8,
        last_error: None,
    };
    client
        .apply_runtime_event(runtime_event(
            6,
            7,
            1,
            RuntimeLifecycleEvent::NetworkStreamStatus {
                input: input(5),
                status: Box::new(status.clone()),
            },
        ))
        .unwrap();
    let view = client.model().view().unwrap();
    let entry = view.network_stream(input(5).to_domain()).unwrap();
    assert_eq!(entry.config, edited);
    assert_eq!(entry.status, status);

    // Removing the input removes its stream.
    client
        .intake(event(7, EventPayload::InputRemoved { input: input(5) }))
        .unwrap();
    assert!(
        client
            .model()
            .view()
            .unwrap()
            .network_stream(input(5).to_domain())
            .is_none()
    );
}

/// IN-001, Protocol 2.47: a capture input added live carries its device
/// and settings into the view, `capture_settings_changed` replaces them,
/// `capture_status` runtime events update its status, and the capture
/// device inventory arrives by runtime event or `capture_devices` reply.
#[test]
#[allow(clippy::too_many_lines)] // one add, edit, status, and inventory round trip
fn capture_inputs_status_and_inventory_replicate() {
    use fm_protocol::{
        CaptureAudio, CaptureDevice, CaptureDevicesMessage, CaptureInputConfig, CaptureInventory,
        CaptureLossReason, CaptureSettings, CaptureSignalState, CaptureStatus,
    };
    let mut client = ready_client(4);
    let event = |revision, payload| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            payload,
        })
    };
    let config = CaptureInputConfig::new("v4l2:usb-1#C920");
    assert_eq!(
        client
            .intake(event(
                5,
                EventPayload::InputAdded {
                    input: input(5),
                    name: "USB camera".into(),
                    network_stream: None,
                    system_audio: None,
                    screen_capture: None,
                    capture: Some(Box::new(config.clone())),
                    still: None,
                },
            ))
            .unwrap(),
        Intake::EventApplied
    );
    let view = client.model().view().unwrap();
    let entry = view
        .capture_input(input(5).to_domain())
        .expect("the added capture input");
    assert_eq!(entry.config, config);
    assert_eq!(entry.status, CaptureStatus::default());
    assert!(view.capture_input(input(1).to_domain()).is_none());
    assert_eq!(view.capture_inventory, None);

    let settings = CaptureSettings {
        audio: CaptureAudio::None,
        ..CaptureSettings::default()
    };
    client
        .intake(event(
            6,
            EventPayload::CaptureSettingsChanged {
                input: input(5),
                settings: Box::new(settings.clone()),
            },
        ))
        .unwrap();
    let status = CaptureStatus {
        state: CaptureSignalState::SignalLost,
        reason: Some(CaptureLossReason::Stalled),
        losses: 1,
        ..CaptureStatus::default()
    };
    client
        .apply_runtime_event(runtime_event(
            6,
            7,
            1,
            RuntimeLifecycleEvent::CaptureStatus {
                input: input(5),
                status: Box::new(status.clone()),
            },
        ))
        .unwrap();
    let view = client.model().view().unwrap();
    let entry = view.capture_input(input(5).to_domain()).unwrap();
    assert_eq!(entry.config.settings, settings);
    assert_eq!(entry.status, status);

    let inventory = CaptureInventory {
        backend: Some("v4l2".into()),
        error: None,
        devices: vec![CaptureDevice {
            stable_key: "v4l2:usb-1#C920".into(),
            name: "C920".into(),
            path: "/dev/video0".into(),
            driver: "uvcvideo".into(),
            bus: "usb-1".into(),
            audio: None,
            formats: Vec::new(),
        }],
    };
    client
        .apply_runtime_event(runtime_event(
            6,
            8,
            1,
            RuntimeLifecycleEvent::CaptureDevices(inventory.clone()),
        ))
        .unwrap();
    assert_eq!(
        client.model().view().unwrap().capture_inventory.as_ref(),
        Some(&inventory)
    );
    let empty = CaptureInventory::default();
    assert_eq!(
        client
            .intake(WireMessage::CaptureDevices(CaptureDevicesMessage {
                request_id: "devices-1".into(),
                inventory: empty.clone(),
            }))
            .unwrap(),
        Intake::CaptureDevicesApplied
    );
    assert_eq!(client.model().capture_inventory(), Some(&empty));
    // The system audio device list (Protocol 2.68) replicates the same way.
    let devices = fm_protocol::SystemAudioInventory {
        supported: true,
        devices: vec![fm_protocol::SystemAudioDevice {
            backend: fm_protocol::SystemAudioBackend::Pulse,
            device: "sink.monitor".into(),
            description: "Monitor of Sink".into(),
            channels: Some(2),
            default: true,
            monitor: true,
        }],
        errors: Vec::new(),
    };
    assert_eq!(client.model().system_audio_inventory(), None);
    assert_eq!(
        client
            .intake(WireMessage::SystemAudioDevices(
                fm_protocol::SystemAudioDevicesMessage {
                    request_id: "sa-1".into(),
                    inventory: devices.clone(),
                }
            ))
            .unwrap(),
        Intake::SystemAudioDevicesApplied
    );
    assert_eq!(client.model().system_audio_inventory(), Some(&devices));
    assert_eq!(
        client
            .model()
            .view()
            .unwrap()
            .system_audio_inventory
            .as_ref(),
        Some(&devices)
    );

    // Removing the input removes its entry.
    client
        .intake(event(7, EventPayload::InputRemoved { input: input(5) }))
        .unwrap();
    assert!(
        client
            .model()
            .view()
            .unwrap()
            .capture_input(input(5).to_domain())
            .is_none()
    );
}

/// AU-011, Protocol 2.55: a system audio input added live carries its
/// audio system and device into the view as idle, `system_audio_status`
/// runtime events update its status, and removing it drops its entry.
#[test]
fn system_audio_inputs_and_their_status_replicate() {
    use fm_protocol::{SystemAudioBackend, SystemAudioConfig, SystemAudioState, SystemAudioStatus};
    let mut client = ready_client(4);
    let event = |revision, payload| {
        WireMessage::Event(EventMessage {
            cursor: EventCursor {
                engine: engine(),
                revision,
            },
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            payload,
        })
    };
    let config = SystemAudioConfig::new(SystemAudioBackend::Alsa, "hw:Loopback,1");
    assert_eq!(
        client
            .intake(event(
                5,
                EventPayload::InputAdded {
                    input: input(5),
                    name: "Loopback".into(),
                    network_stream: None,
                    capture: None,
                    still: None,
                    system_audio: Some(Box::new(config.clone())),
                    screen_capture: None,
                },
            ))
            .unwrap(),
        Intake::EventApplied
    );
    let view = client.model().view().unwrap();
    let entry = view
        .system_audio_input(input(5).to_domain())
        .expect("the added system audio input");
    assert_eq!(entry.config, config);
    assert_eq!(entry.status, SystemAudioStatus::default());
    assert!(view.system_audio_input(input(1).to_domain()).is_none());

    let status = SystemAudioStatus {
        state: SystemAudioState::Reopening {
            attempt: 2,
            next_in_ms: 1_000,
        },
        reopens: 2,
        last_error: Some("No such file or directory".into()),
    };
    client
        .apply_runtime_event(runtime_event(
            5,
            7,
            1,
            RuntimeLifecycleEvent::SystemAudioStatus {
                input: input(5),
                status: Box::new(status.clone()),
            },
        ))
        .unwrap();
    let view = client.model().view().unwrap();
    assert_eq!(
        view.system_audio_input(input(5).to_domain())
            .unwrap()
            .status,
        status
    );
    assert_eq!(view.system_audio_inputs.len(), 1);

    client
        .intake(event(6, EventPayload::InputRemoved { input: input(5) }))
        .unwrap();
    assert!(
        client
            .model()
            .view()
            .unwrap()
            .system_audio_input(input(5).to_domain())
            .is_none()
    );
}

/// Protocol 2.58 (RC-011): a snapshot announces one `script_state` per
/// project script, which complete it after its cue list states;
/// `script_changed` and `script_removed` keep the scripts current.
mod scripts {
    use fm_protocol::{ScriptSpec, ScriptStateMessage, ScriptStatus};

    use super::*;

    fn script(id: u64) -> ScriptStatus {
        ScriptStatus {
            id,
            spec: ScriptSpec {
                name: format!("Script {id}"),
                source: "cut 2".into(),
            },
            source_hidden: false,
        }
    }

    fn script_state(revision: u64, id: u64) -> WireMessage {
        WireMessage::ScriptState(ScriptStateMessage {
            revision,
            script: script(id),
            run: fm_protocol::ScriptRunStatus::Idle,
        })
    }

    fn synchronizing(script_states: u16) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(SnapshotMessage {
                script_states,
                ..snapshot(4)
            }))
            .unwrap();
        client
    }

    fn resyncs(client: &mut Client, message: WireMessage) {
        let error = client.intake(message).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn announced_script_states_complete_the_snapshot_and_events_track_them() {
        let mut client = synchronizing(2);
        assert_eq!(
            client.intake(script_state(4, 3)).unwrap(),
            Intake::ScriptStateApplied
        );
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing {
                mode: SyncMode::Snapshot,
                ..
            }
        ));
        client.intake(script_state(4, 1)).unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);
        let ids = |client: &Client| {
            client
                .model()
                .state()
                .unwrap()
                .scripts()
                .iter()
                .map(|script| script.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&client), [3, 1]);

        client
            .apply_event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 5,
                },
                payload: EventPayload::ScriptRemoved { script: 3 },
            })
            .unwrap();
        client
            .apply_event(EventMessage {
                edit_history: fm_protocol::EditHistoryStatus::EMPTY,
                cursor: EventCursor {
                    engine: engine(),
                    revision: 6,
                },
                payload: EventPayload::ScriptChanged(Box::new(script(7))),
            })
            .unwrap();
        assert_eq!(ids(&client), [1, 7]);
    }

    #[test]
    fn unannounced_or_misplaced_script_states_force_a_fresh_snapshot() {
        // Beyond the announced count.
        let mut client = synchronizing(1);
        client.intake(script_state(4, 1)).unwrap();
        resyncs(&mut client, script_state(4, 2));
        // At another revision.
        resyncs(&mut synchronizing(1), script_state(3, 1));
        // Out of bounds.
        resyncs(&mut synchronizing(1), script_state(4, 0));
    }
}

/// IN-008, IN-009: a snapshot announces one `still_state` per image and
/// slideshow input, in input order, which complete it; `slideshow_page`
/// runtime events move the slide on air, a new listing generation drops
/// the fetched slides, and `slideshow_slides` replies fill the listing.
mod stills {
    use fm_protocol::{
        ImageInputConfig, SlideshowConfig, SlideshowItem, SlideshowPage, SlideshowPageStatus,
        SlideshowSlide, SlideshowSlidesMessage, SlideshowSource, StillStateMessage,
        WireStillSource,
    };

    use super::*;

    fn image() -> WireStillSource {
        WireStillSource::Image(ImageInputConfig::new(
            "asset://logo.png",
            fm_types::OutputScaling::Fit,
        ))
    }

    fn slideshow() -> WireStillSource {
        WireStillSource::Slideshow(SlideshowConfig::new(SlideshowSource::List(vec![
            SlideshowItem::new("asset://a.png"),
            SlideshowItem::new("asset://b.png"),
        ])))
    }

    fn status(index: u32, label: &str, generation: u64) -> SlideshowPageStatus {
        SlideshowPageStatus {
            page: SlideshowPage { index, count: 2 },
            label: label.into(),
            generation,
        }
    }

    fn still_state(revision: u64, id: u128, source: WireStillSource) -> WireMessage {
        WireMessage::StillState(StillStateMessage {
            revision,
            input: input(id),
            source,
            status: SlideshowPageStatus::default(),
        })
    }

    fn synchronizing(still_states: u16) -> Client {
        let mut client = Client::new(config(4)).unwrap();
        client.start_connect().unwrap();
        client.transport_connected().unwrap();
        client.accept_handshake(handshake(4, None)).unwrap();
        client
            .intake(WireMessage::Snapshot(SnapshotMessage {
                still_states,
                ..snapshot(4)
            }))
            .unwrap();
        client
    }

    fn resyncs(client: &mut Client, message: WireMessage) {
        let error = client.intake(message).unwrap_err();
        assert!(
            matches!(error, ClientError::ResyncRequired { .. }),
            "{error:?}"
        );
    }

    fn slides(generation: u64, offset: u32, names: &[&str]) -> WireMessage {
        WireMessage::SlideshowSlides(SlideshowSlidesMessage {
            request_id: "r".into(),
            input: input(2),
            generation,
            offset,
            total: 2,
            slides: names
                .iter()
                .map(|name| SlideshowSlide {
                    asset_uri: format!("asset://{name}"),
                    caption: None,
                })
                .collect(),
        })
    }

    fn page(sequence: u64, id: u128, status: SlideshowPageStatus) -> RuntimeEventMessage {
        runtime_event(
            4,
            1,
            sequence,
            RuntimeLifecycleEvent::SlideshowPage {
                input: input(id),
                status: Box::new(status),
            },
        )
    }

    #[test]
    fn announced_still_states_complete_the_snapshot_and_pages_track_the_slide() {
        let mut client = synchronizing(2);
        assert_eq!(
            client.intake(still_state(4, 1, image())).unwrap(),
            Intake::StillStateApplied
        );
        assert!(matches!(
            client.state(),
            ConnectionState::Synchronizing { .. }
        ));
        client.intake(still_state(4, 2, slideshow())).unwrap();
        assert_eq!(client.state(), &ConnectionState::Ready);

        client
            .apply_runtime_event(page(1, 2, status(1, "b.png", 0)))
            .unwrap();
        // An image has no pages: a stray status is ignored.
        client
            .apply_runtime_event(page(2, 1, status(1, "x", 0)))
            .unwrap();
        let view = client.model().view().unwrap();
        assert_eq!(
            view.still_input(input(2).to_domain()).unwrap().status,
            status(1, "b.png", 0)
        );
        assert_eq!(
            view.still_input(input(1).to_domain()).unwrap().status,
            SlideshowPageStatus::default()
        );

        assert_eq!(
            client.intake(slides(0, 0, &["a.png"])).unwrap(),
            Intake::SlideshowSlidesApplied
        );
        client.intake(slides(0, 1, &["b.png"])).unwrap();
        // A reply for another listing generation is ignored.
        client.intake(slides(7, 0, &["z.png"])).unwrap();
        let listing = |client: &Client| {
            client
                .model()
                .view()
                .unwrap()
                .still_input(input(2).to_domain())
                .unwrap()
                .listing
                .clone()
        };
        let fetched = listing(&client).unwrap();
        assert_eq!(fetched.total, 2);
        assert_eq!(fetched.slides[&0].asset_uri, "asset://a.png");
        assert_eq!(fetched.slides[&1].asset_uri, "asset://b.png");
        assert_eq!(fetched.next_missing(), None);

        // A new listing generation drops the fetched slides.
        client
            .apply_runtime_event(page(3, 2, status(0, "a.png", 1)))
            .unwrap();
        assert_eq!(listing(&client), None);
    }

    #[test]
    fn unannounced_or_misplaced_still_states_force_a_fresh_snapshot() {
        // Beyond the announced count.
        let mut client = synchronizing(1);
        client.intake(still_state(4, 1, image())).unwrap();
        resyncs(&mut client, still_state(4, 2, slideshow()));
        // At another revision.
        resyncs(&mut synchronizing(1), still_state(3, 1, image()));
        // For an input the project does not have.
        resyncs(&mut synchronizing(1), still_state(4, 9, image()));
        // Out of input order.
        let mut client = synchronizing(2);
        client.intake(still_state(4, 2, slideshow())).unwrap();
        resyncs(&mut client, still_state(4, 1, image()));
    }
}
