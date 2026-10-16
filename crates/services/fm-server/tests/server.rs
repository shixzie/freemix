use std::{convert::Infallible, net::IpAddr, num::NonZeroU128};

use fm_auth::{Policy, Principal, Role as AuthRole, SessionId, UserId};
use fm_control::{ControlLimits, ControlService};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, EngineIdentity,
    EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    HandshakeRequest, ManualTransitionStatus, ProtocolVersion, ResumeCursor, Role, ServerIdentity,
    SnapshotMessage, WireInputId,
};
use fm_server::{
    AuthenticationMode, ConfigError, ControlPlane, DisconnectReason, HandshakeError, HealthState,
    Heartbeat, InitialSync, RateLimit, ReadinessState, Server, ServerConfig, ServerMode, Session,
    SessionError, SessionLimits, SessionState, SyncPayload,
};

#[derive(Clone, Debug)]
struct FakeControl {
    engine: EngineIdentity,
    snapshot: SnapshotMessage,
    events: Vec<EventMessage>,
}

impl ControlPlane for FakeControl {
    type Error = Infallible;

    fn initial_sync(&self, cursor: Option<&EventCursor>) -> Result<InitialSync, Self::Error> {
        let payload = if cursor.is_some_and(|cursor| {
            cursor.engine == self.engine && cursor.revision <= self.snapshot.revision
        }) {
            SyncPayload::Resume(
                self.events
                    .iter()
                    .filter(|event| event.cursor.revision > cursor.unwrap().revision)
                    .cloned()
                    .collect(),
            )
        } else {
            SyncPayload::Snapshot(Box::new(self.snapshot.clone()))
        };
        Ok(InitialSync {
            engine: self.engine.clone(),
            current_revision: self.snapshot.revision,
            payload,
        })
    }
}

fn engine() -> EngineIdentity {
    EngineIdentity {
        engine_id: "engine-a".into(),
        state_epoch: 2,
        log_id: "log-a".into(),
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

fn control() -> FakeControl {
    let engine = engine();
    let snapshot = SnapshotMessage {
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        display_outputs: Vec::new(),
        display_output_states: Vec::new(),
        network_streams: Vec::new(),
        engine: engine.clone(),
        revision: 4,
        show_name: "show".into(),
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
        recordings: vec![
            fm_protocol::RecordingStatus {
                recorder: fm_protocol::WireRecorderSlot::new(1).unwrap(),
                desired_active: false,
            },
            fm_protocol::RecordingStatus {
                recorder: fm_protocol::WireRecorderSlot::new(2).unwrap(),
                desired_active: false,
            },
        ],
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
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
    };
    let events = [3, 4]
        .map(|revision| EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: engine.clone(),
                revision,
            },
            payload: EventPayload::DesiredSwitcher {
                program: input(1),
                preview: input(2),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: FadeToBlackState {
                    target_active: false,
                    position: FadeToBlackPosition::LIVE,
                },
                overlays: fm_protocol::OverlayStatus::empty_channels(),
                input_audio_strips: input_audio_strips(),
            },
        })
        .to_vec();
    FakeControl {
        engine,
        snapshot,
        events,
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

fn config() -> ServerConfig {
    ServerConfig::new(
        ServerMode::Production,
        AuthenticationMode::Required,
        IpAddr::from([127, 0, 0, 1]),
        "capabilities-v1",
    )
}

fn principal(role: AuthRole) -> Principal {
    Principal::authenticated(
        UserId::new("alice").unwrap(),
        SessionId::new("session-1").unwrap(),
        [role],
    )
}

fn development_principal(role: AuthRole) -> Principal {
    Principal::development(
        UserId::new("developer").unwrap(),
        SessionId::new("local-session").unwrap(),
        [role],
    )
}

fn hello(role: Role, cursor: Option<EventCursor>) -> HandshakeRequest {
    HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "test".into(),
        client_type: ClientType::Cli,
        desired_role: role,
        resume_cursor: cursor.map(|cursor| ResumeCursor {
            server: ServerIdentity {
                engine_id: cursor.engine.engine_id,
                project_id: "project-9".into(),
                state_epoch: cursor.engine.state_epoch,
                log_id: cursor.engine.log_id,
            },
            revision: cursor.revision,
        }),
    }
}

/// A real control plane over three inputs with empty edit history.
fn control_service() -> ControlService {
    let domain_input = |value: u128| fm_types::InputId::new(NonZeroU128::new(value).unwrap());
    let show = fm_engine::ShowState::new(
        "show",
        (1..=3)
            .map(|value| (domain_input(value), format!("Input {value}")))
            .collect(),
        domain_input(1),
        domain_input(2),
    )
    .unwrap();
    ControlService::new(
        fm_engine::Engine::new(
            show,
            fm_types::FrameRate::new(60, 1).unwrap(),
            fm_clock::ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits::default(),
    )
}

/// Admits `command` with the classes a fresh control plane resolves for it.
fn admit(
    session: &mut Session,
    command: &CommandMessage,
    encoded_bytes: usize,
    now_ms: u64,
) -> Result<(), SessionError> {
    admit_against(session, &control_service(), command, encoded_bytes, now_ms)
}

fn admit_against(
    session: &mut Session,
    control: &ControlService,
    command: &CommandMessage,
    encoded_bytes: usize,
    now_ms: u64,
) -> Result<(), SessionError> {
    let required = control.required_classes(&command.payload);
    session.admit_command(command, required, encoded_bytes, now_ms)
}

fn ready_server(config: ServerConfig) -> Server<FakeControl> {
    let mut server = Server::new(config, control()).unwrap();
    server.mark_ready().unwrap();
    server
}

#[test]
fn non_current_versions_are_rejected() {
    let server = ready_server(config());
    let mut hello = hello(Role::Viewer, None);
    hello.protocol = ProtocolVersion::new(99, 0);

    assert!(matches!(
        server.handshake(&hello, &principal(AuthRole::Viewer), 0),
        Err(HandshakeError::ProtocolMismatch)
    ));
}

#[test]
fn development_auth_requires_development_mode_and_loopback() {
    let production = ServerConfig::new(
        ServerMode::Production,
        AuthenticationMode::Development,
        IpAddr::from([127, 0, 0, 1]),
        "digest",
    );
    assert_eq!(
        Server::new(production, control()).unwrap_err(),
        ConfigError::DevelopmentAuthInProduction
    );

    let exposed = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        IpAddr::from([0, 0, 0, 0]),
        "digest",
    );
    assert_eq!(
        Server::new(exposed, control()).unwrap_err(),
        ConfigError::DevelopmentAuthRequiresLoopback
    );
}

#[test]
fn production_denies_development_principals() {
    let server = ready_server(config());
    assert!(matches!(
        server.handshake(
            &hello(Role::Admin, None),
            &development_principal(AuthRole::Admin),
            0,
        ),
        Err(HandshakeError::DevelopmentPrincipalDenied)
    ));
}

#[test]
fn handshake_delegates_snapshot_and_resume_selection() {
    let server = ready_server(config());
    let snapshot = server
        .handshake(&hello(Role::Viewer, None), &principal(AuthRole::Viewer), 10)
        .unwrap();
    assert_eq!(snapshot.server_hello.protocol, CURRENT_PROTOCOL_VERSION);
    assert!(!snapshot.server_hello.resume);
    assert!(matches!(snapshot.sync, SyncPayload::Snapshot(_)));

    let cursor = EventCursor {
        engine: engine(),
        revision: 2,
    };
    let resumed = server
        .handshake(
            &hello(Role::Viewer, Some(cursor)),
            &principal(AuthRole::Viewer),
            10,
        )
        .unwrap();
    assert!(resumed.server_hello.resume);
    assert!(matches!(resumed.sync, SyncPayload::Resume(events) if events.len() == 2));
}

#[test]
fn non_contiguous_resume_is_rejected_before_session_exposure() {
    let mut control = control();
    control.events.retain(|event| event.cursor.revision == 4);
    let mut server = Server::new(config(), control).unwrap();
    server.mark_ready().unwrap();
    let cursor = EventCursor {
        engine: engine(),
        revision: 2,
    };

    assert!(matches!(
        server.handshake(
            &hello(Role::Viewer, Some(cursor)),
            &principal(AuthRole::Viewer),
            0,
        ),
        Err(HandshakeError::InvalidControlSync)
    ));
}

#[test]
fn desired_role_and_commands_are_authorized_to_the_granted_scope() {
    let server = ready_server(config());
    assert!(matches!(
        server.handshake(
            &hello(Role::Operator, None),
            &principal(AuthRole::Viewer),
            0,
        ),
        Err(HandshakeError::RoleDenied(Role::Operator))
    ));

    let mut outcome = server
        .handshake(&hello(Role::Viewer, None), &principal(AuthRole::Admin), 0)
        .unwrap();
    let command = command(CommandPayload::Cut);
    assert!(matches!(
        admit(&mut outcome.session, &command, 10, 0),
        Err(SessionError::Authorization(_))
    ));
}

#[test]
fn wipe_uses_transition_authorization_and_fade_rate_accounting() {
    let server = ready_server(config());
    let wipe = command(CommandPayload::Wipe { duration_frames: 3 });
    let mut viewer = server
        .handshake(&hello(Role::Viewer, None), &principal(AuthRole::Viewer), 0)
        .unwrap()
        .session;
    assert!(matches!(
        admit(&mut viewer, &wipe, 10, 0),
        Err(SessionError::Authorization(_))
    ));

    let limits = SessionLimits {
        inbound_commands: RateLimit::new(1, 100),
        ..SessionLimits::default()
    };
    let current_config = ServerConfig::new(
        ServerMode::Production,
        AuthenticationMode::Required,
        IpAddr::from([127, 0, 0, 1]),
        "capabilities-v1",
    )
    .with_session_limits(limits);
    let server = ready_server(current_config);
    let current_hello = hello(Role::Operator, None);
    let mut operator = server
        .handshake(&current_hello, &principal(AuthRole::Operator), 0)
        .unwrap()
        .session;
    let mut wipe = wipe;
    wipe.protocol = CURRENT_PROTOCOL_VERSION;
    admit(&mut operator, &wipe, 10, 0).unwrap();
    operator.command_completed().unwrap();
    let mut fade = command(CommandPayload::Fade { duration_frames: 3 });
    fade.protocol = CURRENT_PROTOCOL_VERSION;
    assert_eq!(
        admit(&mut operator, &fade, 10, 0),
        Err(SessionError::InboundRateLimited)
    );
}

#[test]
fn title_take_with_fields_also_needs_project_edit_authority() {
    let server = ready_server(config());
    let take = |fields: Vec<fm_protocol::TitleFieldUpdate>| {
        let mut message = command(CommandPayload::TakeTitle {
            input: fm_protocol::WireInputId::new(core::num::NonZeroU128::new(4).unwrap()),
            channel: fm_protocol::WireOverlayChannelId::new(1).unwrap(),
            base_revision: 0,
            fields,
        });
        message.protocol = CURRENT_PROTOCOL_VERSION;
        message
    };
    let fields = vec![fm_protocol::TitleFieldUpdate {
        field: fm_protocol::WireTitleFieldId::new(core::num::NonZeroU128::new(1).unwrap()),
        value: fm_protocol::TitleFieldValue::Number(1),
    }];
    let mut operator = server
        .handshake(
            &hello(Role::Operator, None),
            &principal(AuthRole::Operator),
            0,
        )
        .unwrap()
        .session;
    assert!(matches!(
        admit(&mut operator, &take(fields.clone()), 10, 0),
        Err(SessionError::Authorization(_))
    ));
    admit(&mut operator, &take(Vec::new()), 10, 0).unwrap();
    operator.command_completed().unwrap();

    let mut admin = server
        .handshake(&hello(Role::Admin, None), &principal(AuthRole::Admin), 0)
        .unwrap()
        .session;
    admit(&mut admin, &take(fields), 10, 0).unwrap();
}

#[test]
fn undo_and_redo_also_need_the_targeted_edit_class_at_the_session() {
    let server = ready_server(config());
    let session_for = |role: Role, auth_role: AuthRole| {
        server
            .handshake(&hello(role, None), &principal(auth_role), 0)
            .unwrap()
            .session
    };
    let mut graphics = session_for(Role::Graphics, AuthRole::Graphics);
    let mut operator = session_for(Role::Operator, AuthRole::Operator);
    let mut admin = session_for(Role::Admin, AuthRole::Admin);
    let undo = command(CommandPayload::Undo {
        expected_edit: None,
    });
    let redo = command(CommandPayload::Redo {
        expected_edit: None,
    });
    let mut control = control_service();
    let submit = |control: &mut ControlService, id: &str, payload: CommandPayload| {
        let mut message = command(payload);
        message.id = id.into();
        message.idempotency_key = format!("{id}-key");
        assert!(
            control
                .submit(&principal(AuthRole::Admin), message, 0)
                .unwrap()
                .is_accepted(),
            "{id}"
        );
    };

    // A rename is EditProject only: Graphics may undo it, Operator may not.
    submit(
        &mut control,
        "rename",
        CommandPayload::RenameInput {
            input: input(1),
            name: "Renamed".into(),
        },
    );
    admit_against(&mut graphics, &control, &undo, 10, 0).unwrap();
    graphics.command_completed().unwrap();
    assert!(matches!(
        admit_against(&mut operator, &control, &undo, 10, 0),
        Err(SessionError::Authorization(_))
    ));

    // A Stinger slot change is Transition: Graphics may undo its own class but
    // not revert the switcher configuration it could not change directly.
    let stinger = CommandPayload::ConfigureStinger {
        slot: fm_protocol::WireStingerSlotId::new(2).unwrap(),
        media_input: input(3),
        preload: false,
        cut_point_frames: 6,
        audio_policy: fm_protocol::StingerAudioPolicy::StingerOnly,
        missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Cut,
    };
    submit(&mut control, "stinger", stinger);
    assert!(matches!(
        admit_against(&mut graphics, &control, &undo, 10, 0),
        Err(SessionError::Authorization(_))
    ));
    assert!(matches!(
        admit_against(&mut operator, &control, &undo, 10, 0),
        Err(SessionError::Authorization(_))
    ));
    admit_against(&mut admin, &control, &undo, 10, 0).unwrap();
    admin.command_completed().unwrap();

    // Once the Stinger change is undone, redoing it needs Transition too,
    // while the rename beneath it is undoable by Graphics again.
    submit(
        &mut control,
        "undo-stinger",
        CommandPayload::Undo {
            expected_edit: None,
        },
    );
    assert!(matches!(
        admit_against(&mut graphics, &control, &redo, 10, 0),
        Err(SessionError::Authorization(_))
    ));
    admit_against(&mut admin, &control, &redo, 10, 0).unwrap();
    admin.command_completed().unwrap();
    admit_against(&mut graphics, &control, &undo, 10, 0).unwrap();
    graphics.command_completed().unwrap();
}

#[test]
fn heartbeat_tracks_cursor_and_times_out() {
    let server = ready_server(config());
    let mut session = server
        .handshake(
            &hello(Role::Viewer, None),
            &principal(AuthRole::Viewer),
            100,
        )
        .unwrap()
        .session;
    let cursor = EventCursor {
        engine: engine(),
        revision: 4,
    };
    session
        .record_heartbeat(
            Heartbeat {
                last_applied: Some(cursor.clone()),
                clock_sample_ms: 90,
            },
            200,
        )
        .unwrap();
    assert_eq!(session.heartbeat().last_applied.as_ref(), Some(&cursor));
    assert_eq!(session.heartbeat().received_total, 1);
    assert!(session.check_heartbeat(15_199).is_ok());
    assert_eq!(
        session.check_heartbeat(15_200),
        Err(SessionError::Disconnected(
            DisconnectReason::HeartbeatTimeout
        ))
    );
}

#[test]
fn command_rates_and_outbound_buffers_are_bounded() {
    let limits = SessionLimits {
        max_command_bytes: 32,
        max_inflight_commands: 1,
        inbound_commands: RateLimit::new(1, 100),
        max_outbound_messages: 2,
        max_outbound_bytes: 10,
        outbound_messages: RateLimit::new(10, 100),
        heartbeat_timeout_ms: 1_000,
        title_previews: RateLimit::new(10, 1_000),
    };
    let server = ready_server(config().with_session_limits(limits));
    let mut session = server
        .handshake(
            &hello(Role::Operator, None),
            &principal(AuthRole::Operator),
            0,
        )
        .unwrap()
        .session;
    let command = command(CommandPayload::Cut);

    assert_eq!(
        admit(&mut session, &command, 33, 0),
        Err(SessionError::CommandTooLarge {
            size: 33,
            maximum: 32,
        })
    );
    admit(&mut session, &command, 20, 0).unwrap();
    assert_eq!(
        admit(&mut session, &command, 20, 0),
        Err(SessionError::TooManyInflightCommands)
    );
    session.command_completed().unwrap();
    assert_eq!(
        admit(&mut session, &command, 20, 0),
        Err(SessionError::InboundRateLimited)
    );
    admit(&mut session, &command, 20, 100).unwrap();

    session.queue_outbound(5, 0).unwrap();
    session.queue_outbound(5, 0).unwrap();
    assert_eq!(session.accounting().outbound_bytes_queued, 10);
    assert_eq!(
        session.queue_outbound(1, 0),
        Err(SessionError::Disconnected(DisconnectReason::SlowClient))
    );
    assert_eq!(
        session.state(),
        SessionState::Disconnected(DisconnectReason::SlowClient)
    );
}

#[test]
fn outbound_message_rate_limit_is_retryable() {
    let limits = SessionLimits {
        max_command_bytes: 32,
        max_inflight_commands: 1,
        inbound_commands: RateLimit::new(1, 100),
        max_outbound_messages: 2,
        max_outbound_bytes: 10,
        outbound_messages: RateLimit::new(1, 100),
        heartbeat_timeout_ms: 1_000,
        title_previews: RateLimit::new(10, 1_000),
    };
    let server = ready_server(config().with_session_limits(limits));
    let mut session = server
        .handshake(&hello(Role::Viewer, None), &principal(AuthRole::Viewer), 0)
        .unwrap()
        .session;

    session.queue_outbound(5, 0).unwrap();
    assert_eq!(
        session.queue_outbound(5, 0),
        Err(SessionError::OutboundRateLimited)
    );
    assert_eq!(session.state(), SessionState::Connected);
    session.outbound_delivered().unwrap();
    session.queue_outbound(5, 100).unwrap();
}

#[test]
fn discarded_outbound_message_releases_its_exact_bytes() {
    let server = ready_server(config());
    let mut session = server
        .handshake(&hello(Role::Viewer, None), &principal(AuthRole::Viewer), 0)
        .unwrap()
        .session;

    session.queue_outbound(3, 0).unwrap();
    session.queue_outbound(5, 0).unwrap();
    session.queue_outbound(2, 0).unwrap();

    assert_eq!(session.discard_outbound(1).unwrap(), 5);
    assert_eq!(session.accounting().outbound_messages_queued, 2);
    assert_eq!(session.accounting().outbound_bytes_queued, 5);
    assert_eq!(session.outbound_delivered().unwrap(), 3);
    assert_eq!(session.outbound_delivered().unwrap(), 2);
}

#[test]
fn health_and_readiness_have_stable_lifecycle_states() {
    let mut server = Server::new(config(), control()).unwrap();
    assert_eq!(server.status().health(), HealthState::Healthy);
    assert_eq!(server.status().readiness(), ReadinessState::Starting);
    assert!(matches!(
        server.handshake(&hello(Role::Viewer, None), &principal(AuthRole::Viewer), 0),
        Err(HandshakeError::NotReady(ReadinessState::Starting))
    ));

    server.mark_ready().unwrap();
    assert_eq!(server.status().readiness(), ReadinessState::Ready);
    server.begin_draining().unwrap();
    assert_eq!(server.status().health(), HealthState::Healthy);
    assert_eq!(server.status().readiness(), ReadinessState::Draining);
    assert!(server.mark_ready().is_err());
    server.mark_unhealthy();
    assert_eq!(server.status().health(), HealthState::Unhealthy);
    assert_eq!(server.status().readiness(), ReadinessState::Unhealthy);
}

fn command(payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "command-1".into(),
        idempotency_key: "alice-1".into(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    }
}

#[test]
fn rate_window_reopens_when_the_clock_steps_backwards() {
    let limits = SessionLimits {
        inbound_commands: RateLimit::new(1, 1_000),
        outbound_messages: RateLimit::new(1, 1_000),
        ..SessionLimits::default()
    };
    let server = ready_server(config().with_session_limits(limits));
    let mut session = server
        .handshake(
            &hello(Role::Operator, None),
            &principal(AuthRole::Operator),
            600_000,
        )
        .unwrap()
        .session;
    let command = command(CommandPayload::Cut);

    admit(&mut session, &command, 20, 600_000).unwrap();
    session.command_completed().unwrap();
    session.queue_outbound(5, 600_000).unwrap();
    assert_eq!(
        admit(&mut session, &command, 20, 600_000),
        Err(SessionError::InboundRateLimited)
    );

    // The wall clock steps back ten minutes: the limiter must not stay shut
    // until it catches up again.
    admit(&mut session, &command, 20, 0).unwrap();
    session.queue_outbound(5, 0).unwrap();
}

#[test]
fn title_previews_need_edit_project_and_have_their_own_rate() {
    use fm_protocol::{TitlePreviewRequest, TitlePreviewSample};

    let limits = SessionLimits {
        title_previews: RateLimit::new(2, 100),
        ..SessionLimits::default()
    };
    let server = ready_server(config().with_session_limits(limits));
    let session_for = |role: Role, auth_role: AuthRole| {
        server
            .handshake(&hello(role, None), &principal(auth_role), 0)
            .unwrap()
            .session
    };
    let request = TitlePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "preview-1".into(),
        input: input(3),
        sample: TitlePreviewSample::Hold,
        width: 64,
        height: 36,
        clip: None,
    };

    let mut operator = session_for(Role::Operator, AuthRole::Operator);
    assert!(matches!(
        operator.admit_title_preview(&request, 10, 0),
        Err(SessionError::Authorization(_))
    ));

    let mut graphics = session_for(Role::Graphics, AuthRole::Graphics);
    graphics.admit_title_preview(&request, 10, 0).unwrap();
    graphics.admit_title_preview(&request, 10, 0).unwrap();
    assert_eq!(
        graphics.admit_title_preview(&request, 10, 50),
        Err(SessionError::TitlePreviewRateLimited)
    );
    // Previews never hold a command slot, and the window reopens.
    assert_eq!(graphics.accounting().inbound_commands_inflight, 0);
    graphics.admit_title_preview(&request, 10, 100).unwrap();

    let stale = TitlePreviewRequest {
        protocol: ProtocolVersion::new(2, 28),
        ..request
    };
    assert!(matches!(
        graphics.admit_title_preview(&stale, 10, 100),
        Err(SessionError::ProtocolMismatch { .. })
    ));

    let zero = SessionLimits {
        title_previews: RateLimit::new(0, 1_000),
        ..SessionLimits::default()
    };
    assert_eq!(
        config().with_session_limits(zero).validate(),
        Err(ConfigError::ZeroLimit("title_previews"))
    );
}

/// Protocol 2.32: scene previews are admitted like title previews and share
/// their rate.
#[test]
fn scene_previews_need_edit_project_and_share_the_preview_rate() {
    use fm_protocol::{ScenePreviewRequest, TitlePreviewRequest, TitlePreviewSample};

    let limits = SessionLimits {
        title_previews: RateLimit::new(2, 100),
        ..SessionLimits::default()
    };
    let server = ready_server(config().with_session_limits(limits));
    let session_for = |role: Role, auth_role: AuthRole| {
        server
            .handshake(&hello(role, None), &principal(auth_role), 0)
            .unwrap()
            .session
    };
    let scene = ScenePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "preview-1".into(),
        input: input(5),
        sample: TitlePreviewSample::In { frame: 3 },
        width: 64,
        height: 36,
        clip: None,
    };
    let title = TitlePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "preview-2".into(),
        input: input(3),
        sample: TitlePreviewSample::Hold,
        width: 64,
        height: 36,
        clip: None,
    };

    let mut operator = session_for(Role::Operator, AuthRole::Operator);
    assert!(matches!(
        operator.admit_scene_preview(&scene, 10, 0),
        Err(SessionError::Authorization(_))
    ));

    let mut graphics = session_for(Role::Graphics, AuthRole::Graphics);
    graphics.admit_scene_preview(&scene, 10, 0).unwrap();
    graphics.admit_title_preview(&title, 10, 0).unwrap();
    assert_eq!(
        graphics.admit_scene_preview(&scene, 10, 50),
        Err(SessionError::TitlePreviewRateLimited)
    );
    assert_eq!(graphics.accounting().inbound_commands_inflight, 0);
    graphics.admit_scene_preview(&scene, 10, 100).unwrap();
    let stale = ScenePreviewRequest {
        protocol: ProtocolVersion::new(2, 31),
        ..scene
    };
    assert!(matches!(
        graphics.admit_scene_preview(&stale, 10, 100),
        Err(SessionError::ProtocolMismatch { .. })
    ));
}
