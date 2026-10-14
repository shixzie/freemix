//! Phone tally model: realized lights, labels, offline safety, the
//! reconnect/heartbeat driver, and role-limited shortcuts. Every server
//! record is round-tripped through the current wire codec first.

use core::num::NonZeroU128;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CapabilityReportSummary, CommandPayload, EngineIdentity,
    ErrorMessage, EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState,
    HandshakeOutcome, HandshakeResponse, HeartbeatAcknowledgementMessage, InputAudioStripStatus,
    InputStatus, ManualTransitionKind, ManualTransitionPosition, ManualTransitionState,
    ManualTransitionStatus, OverlayStatus, ProtocolVersion, RecordingStatus, Role,
    RuntimeEventMessage, RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage, SnapshotReason,
    StructuredError, WireInputId, WireMessage, WireRecorderSlot, decode_line, encode_line,
};
use fm_types::{InputId, ProjectId};
use freemix_web::{
    TALLY_HEARTBEAT_ACK_TIMEOUT_MS, TALLY_HEARTBEAT_INTERVAL_MS, TALLY_INITIAL_BACKOFF_MS,
    TALLY_MAX_BACKOFF_MS, TALLY_SYNC_TIMEOUT_MS, TallyAction, TallyConnection, TallyIndicator,
    TallyLight, TallyLink, TallyLock, TallyScreen, TallyShortcut, TallyStop,
    TransitionControlState, client_config,
};

const CAMERA: u128 = 11;
const SLIDES: u128 = 12;
const GUEST: u128 = 13;

fn wire(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn input(value: u128) -> InputId {
    wire(value).to_domain()
}

fn project_id() -> ProjectId {
    ProjectId::new(NonZeroU128::new(77).unwrap())
}

fn engine() -> EngineIdentity {
    EngineIdentity {
        engine_id: "engine-tally".to_owned(),
        state_epoch: 2,
        log_id: "log-tally".to_owned(),
    }
}

fn server() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-tally".to_owned(),
        project_id: project_id().to_string(),
        state_epoch: 2,
        log_id: "log-tally".to_owned(),
    }
}

/// Round-trips a server record through the exact current wire codec.
#[allow(clippy::needless_pass_by_value)] // takes ownership to read like a send
fn on_wire(message: WireMessage) -> WireMessage {
    decode_line(&encode_line(&message).unwrap()).unwrap()
}

fn permissions(role: Role) -> Vec<String> {
    let names: &[&str] = match role {
        Role::Viewer => &["view_status"],
        Role::Operator => &[
            "view_status",
            "select_preview",
            "transition",
            "control_audio",
        ],
        _ => unreachable!("tests use viewer and operator"),
    };
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn handshake(role: Role, revision: u64) -> WireMessage {
    handshake_with(
        CURRENT_PROTOCOL_VERSION,
        role,
        revision,
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::NoCursor,
        },
    )
}

fn handshake_with(
    protocol: ProtocolVersion,
    role: Role,
    revision: u64,
    outcome: HandshakeOutcome,
) -> WireMessage {
    on_wire(WireMessage::HandshakeResponse(HandshakeResponse {
        protocol,
        granted_role: role,
        permissions: permissions(role),
        capabilities: CapabilityReportSummary {
            digest: "sha256:tally".to_owned(),
            total: 1,
            available: 1,
            degraded: 0,
            unavailable: 0,
        },
        server: server(),
        current_revision: revision,
        outcome,
    }))
}

fn live() -> FadeToBlackState {
    FadeToBlackState {
        target_active: false,
        position: FadeToBlackPosition::LIVE,
    }
}

fn strips() -> Vec<InputAudioStripStatus> {
    [CAMERA, SLIDES, GUEST]
        .into_iter()
        .map(|value| InputAudioStripStatus {
            input: wire(value),
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        })
        .collect()
}

fn snapshot(revision: u64, program: u128, preview: u128) -> WireMessage {
    snapshot_locked(
        revision,
        program,
        preview,
        fm_protocol::EditLockStatus::default(),
    )
}

fn snapshot_locked(
    revision: u64,
    program: u128,
    preview: u128,
    edit_locks: fm_protocol::EditLockStatus,
) -> WireMessage {
    on_wire(WireMessage::Snapshot(SnapshotMessage {
        engine: engine(),
        revision,
        show_name: "Morning Show".to_owned(),
        inputs: [
            (CAMERA, "Camera 1"),
            (SLIDES, "Slides"),
            (GUEST, "Guest / Remote"),
        ]
        .into_iter()
        .map(|(value, name)| InputStatus {
            input: wire(value),
            name: name.to_owned(),
        })
        .collect(),
        outputs: Vec::new(),
        input_audio_strips: strips(),
        desired_program: wire(program),
        desired_preview: wire(preview),
        realized_program: wire(program),
        realized_preview: wire(preview),
        desired_manual_transition: ManualTransitionStatus::Inactive,
        realized_manual_transition: ManualTransitionStatus::Inactive,
        desired_fade_to_black: live(),
        realized_fade_to_black: live(),
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: [1, 2]
            .into_iter()
            .map(|slot| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active: false,
            })
            .collect(),
        desired_overlays: OverlayStatus::empty_channels(),
        realized_overlays: OverlayStatus::empty_channels(),
        edit_locks,
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        titles: Vec::new(),
    }))
}

fn switcher_event(revision: u64, program: u128, preview: u128) -> WireMessage {
    switcher_event_with(revision, program, preview, ManualTransitionStatus::Inactive)
}

fn switcher_event_with(
    revision: u64,
    program: u128,
    preview: u128,
    manual_transition: ManualTransitionStatus,
) -> WireMessage {
    on_wire(WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: EventCursor {
            engine: engine(),
            revision,
        },
        payload: EventPayload::DesiredSwitcher {
            program: wire(program),
            preview: wire(preview),
            manual_transition,
            fade_to_black: live(),
            overlays: OverlayStatus::empty_channels(),
            input_audio_strips: strips(),
        },
    }))
}

fn realized(
    revision: u64,
    sequence: u64,
    manual_transition: ManualTransitionStatus,
) -> WireMessage {
    on_wire(WireMessage::RuntimeEvent(RuntimeEventMessage {
        server: server(),
        revision,
        generation: 1,
        sequence,
        event: RuntimeLifecycleEvent::Realized {
            domain: "switcher".to_owned(),
            manual_transition,
            fade_to_black: live(),
        },
    }))
}

fn rename(revision: u64, value: u128, name: &str) -> WireMessage {
    on_wire(WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: EventCursor {
            engine: engine(),
            revision,
        },
        payload: EventPayload::InputRenamed {
            input: wire(value),
            name: name.to_owned(),
        },
    }))
}

fn durable(revision: u64, payload: EventPayload) -> WireMessage {
    on_wire(WireMessage::Event(EventMessage {
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        cursor: EventCursor {
            engine: engine(),
            revision,
        },
        payload,
    }))
}

fn edit_lock(revision: u64, scope: fm_protocol::EditLockScope, locked: bool) -> WireMessage {
    durable(revision, EventPayload::EditLockChanged { scope, locked })
}

/// Unboxes a record the driver asked the transport to send.
fn sent(action: Option<TallyAction>) -> Option<WireMessage> {
    match action? {
        TallyAction::Send(message) => Some(*message),
        TallyAction::OpenTransport | TallyAction::CloseTransport => None,
    }
}

fn link(role: Role) -> TallyLink {
    TallyLink::new(client_config(role, "phone-tally-1", project_id())).unwrap()
}

/// Opens a transport at `now` and synchronizes a snapshot.
fn connect(link: &mut TallyLink, role: Role, now: u64, program: u128, preview: u128) {
    assert_eq!(
        link.poll(now).or_else(|| link.start()),
        Some(TallyAction::OpenTransport)
    );
    let Some(WireMessage::HandshakeRequest(request)) =
        sent(Some(link.transport_opened(now).unwrap()))
    else {
        panic!("an opened transport must send the handshake first");
    };
    assert_eq!(request.desired_role, role);
    assert_eq!(request.client_type, fm_protocol::ClientType::Web);
    assert_eq!(link.receive(handshake(role, 4), now), None);
    assert_eq!(link.receive(snapshot(4, program, preview), now), None);
    assert_eq!(link.connection(now), TallyConnection::Live);
}

fn screen(selected: u128) -> TallyScreen {
    TallyScreen {
        selected: Some(input(selected)),
        full_screen: false,
    }
}

fn indicator(link: &TallyLink, selected: u128, now: u64) -> TallyIndicator {
    link.presentation(&screen(selected), now)
        .selected
        .expect("selected input is in the catalog")
        .indicator
}

#[test]
fn lights_and_labels_follow_realized_state_only() {
    let mut link = link(Role::Viewer);
    connect(&mut link, Role::Viewer, 0, CAMERA, SLIDES);

    let presentation = link.presentation(&screen(GUEST), 0);
    assert_eq!(presentation.show_name.as_deref(), Some("Morning Show"));
    let lights = presentation
        .board
        .entries()
        .iter()
        .map(|entry| (entry.label.as_str(), entry.light))
        .collect::<Vec<_>>();
    assert_eq!(
        lights,
        [
            ("Camera 1", TallyLight::Program),
            ("Slides", TallyLight::Preview),
            ("Guest / Remote", TallyLight::Idle),
        ]
    );
    let selected = presentation.selected.unwrap();
    assert_eq!(selected.label, "Guest / Remote");
    assert_eq!(selected.indicator, TallyIndicator::Light(TallyLight::Idle));

    // A durable desired cut is intent, not air: the light waits for realization.
    assert_eq!(link.receive(switcher_event(5, SLIDES, CAMERA), 10), None);
    assert_eq!(
        indicator(&link, CAMERA, 10),
        TallyIndicator::Light(TallyLight::Program)
    );
    assert_eq!(
        link.receive(realized(5, 1, ManualTransitionStatus::Inactive), 20),
        None
    );
    assert_eq!(
        indicator(&link, CAMERA, 20),
        TallyIndicator::Light(TallyLight::Preview)
    );
    assert_eq!(
        indicator(&link, SLIDES, 20),
        TallyIndicator::Light(TallyLight::Program)
    );

    // Labels follow durable renames.
    assert_eq!(link.receive(rename(6, GUEST, "Guest"), 30), None);
    assert_eq!(
        link.presentation(&screen(GUEST), 30)
            .selected
            .unwrap()
            .label,
        "Guest"
    );
    assert_eq!(
        TallyIndicator::Light(TallyLight::Program).accessibility_label(),
        "Tally: on air in Program"
    );
    assert_eq!(TallyLight::Program.label(), "ON AIR");
}

#[test]
fn both_sides_of_a_realized_manual_transition_are_on_air() {
    let mut link = link(Role::Viewer);
    connect(&mut link, Role::Viewer, 0, CAMERA, SLIDES);
    let manual = ManualTransitionStatus::Active(ManualTransitionState {
        kind: ManualTransitionKind::Fade,
        from: wire(CAMERA),
        to: wire(SLIDES),
        interval_start: ManualTransitionPosition::START,
        position: ManualTransitionPosition::new(4_000).unwrap(),
    });
    assert_eq!(
        link.receive(switcher_event_with(5, CAMERA, SLIDES, manual), 0),
        None
    );
    assert_eq!(
        indicator(&link, SLIDES, 0),
        TallyIndicator::Light(TallyLight::Preview)
    );
    assert_eq!(link.receive(realized(5, 1, manual), 0), None);
    for value in [CAMERA, SLIDES] {
        assert_eq!(
            indicator(&link, value, 0),
            TallyIndicator::Light(TallyLight::Program)
        );
    }
    assert_eq!(
        indicator(&link, GUEST, 0),
        TallyIndicator::Light(TallyLight::Idle)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn padlocks_follow_snapshots_and_events_without_changing_lights() {
    use fm_protocol::{EditLockScope, EditLockStatus};

    let mut link = link(Role::Viewer);
    assert_eq!(link.start(), Some(TallyAction::OpenTransport));
    link.transport_opened(0).unwrap();
    assert_eq!(link.receive(handshake(Role::Viewer, 4), 0), None);
    let locks = EditLockStatus {
        project: false,
        inputs: vec![wire(GUEST)],
    };
    assert_eq!(
        link.receive(snapshot_locked(4, CAMERA, SLIDES, locks), 0),
        None
    );
    let presentation = link.presentation(&screen(GUEST), 0);
    assert!(!presentation.board.project_locked());
    let locked = presentation
        .board
        .entries()
        .iter()
        .map(|entry| (entry.label.as_str(), entry.locked))
        .collect::<Vec<_>>();
    assert_eq!(
        locked,
        [
            ("Camera 1", false),
            ("Slides", false),
            ("Guest / Remote", true)
        ]
    );
    let selected = presentation.selected.unwrap();
    assert_eq!(selected.lock, Some(TallyLock::Input));
    assert_eq!(selected.indicator, TallyIndicator::Light(TallyLight::Idle));
    let camera = link.presentation(&screen(CAMERA), 0).selected.unwrap();
    assert_eq!(camera.lock, None);
    assert_eq!(camera.indicator, TallyIndicator::Light(TallyLight::Program));

    // The project padlock covers every input and wins over an input lock;
    // lights are unaffected.
    assert_eq!(
        link.receive(edit_lock(5, EditLockScope::Project, true), 10),
        None
    );
    let presentation = link.presentation(&screen(CAMERA), 10);
    assert!(presentation.board.project_locked());
    let camera = presentation.selected.unwrap();
    assert_eq!(camera.lock, Some(TallyLock::Project));
    assert_eq!(camera.indicator, TallyIndicator::Light(TallyLight::Program));
    assert_eq!(
        link.presentation(&screen(GUEST), 10).selected.unwrap().lock,
        Some(TallyLock::Project)
    );

    // Releasing the project lock reveals the input lock again; releasing
    // that clears the badge.
    assert_eq!(
        link.receive(edit_lock(6, EditLockScope::Project, false), 20),
        None
    );
    assert_eq!(
        link.presentation(&screen(GUEST), 20).selected.unwrap().lock,
        Some(TallyLock::Input)
    );
    assert_eq!(
        link.receive(edit_lock(7, EditLockScope::Input(wire(GUEST)), false), 30),
        None
    );
    assert_eq!(
        link.presentation(&screen(GUEST), 30).selected.unwrap().lock,
        None
    );

    // Removing a locked (idle) input drops its lock with the input.
    assert_eq!(
        link.receive(edit_lock(8, EditLockScope::Input(wire(GUEST)), true), 40),
        None
    );
    assert!(
        link.presentation(&screen(GUEST), 40)
            .board
            .entry(input(GUEST))
            .unwrap()
            .locked
    );
    assert_eq!(
        link.receive(
            durable(9, EventPayload::InputRemoved { input: wire(GUEST) }),
            50
        ),
        None
    );
    let presentation = link.presentation(&screen(GUEST), 50);
    assert!(presentation.board.entry(input(GUEST)).is_none());
    assert!(
        presentation
            .board
            .entries()
            .iter()
            .all(|entry| !entry.locked)
    );

    // While not live the lock state is unknown, like the light.
    assert_eq!(
        link.receive(edit_lock(10, EditLockScope::Project, true), 60),
        None
    );
    link.transport_closed(70);
    let offline = link.presentation(&screen(CAMERA), 70).selected.unwrap();
    assert_eq!(offline.indicator, TallyIndicator::Offline);
    assert_eq!(offline.lock, None);
    assert_eq!(TallyLock::Project.label(), "Project locked");
    assert_eq!(
        TallyLock::Input.accessibility_label(),
        "Edits locked: Input locked"
    );
}

#[test]
fn heartbeats_keep_the_link_alive_and_a_missed_ack_reconnects_with_backoff() {
    let mut link = link(Role::Viewer);
    assert_eq!(link.connection(0), TallyConnection::Idle);
    connect(&mut link, Role::Viewer, 0, CAMERA, SLIDES);

    assert_eq!(link.poll(TALLY_HEARTBEAT_INTERVAL_MS - 1), None);
    let now = TALLY_HEARTBEAT_INTERVAL_MS;
    let Some(WireMessage::Heartbeat(heartbeat)) = sent(link.poll(now)) else {
        panic!("heartbeat due");
    };
    assert_eq!(heartbeat.server, server());
    assert_eq!(link.poll(now), None);
    let ack = on_wire(WireMessage::HeartbeatAcknowledgement(
        HeartbeatAcknowledgementMessage {
            server: server(),
            heartbeat_sequence: heartbeat.sequence,
            received_at_ms: 1,
        },
    ));
    assert_eq!(link.receive(ack, now + 10), None);
    // The ack cleared the deadline; the next heartbeat is one interval later.
    assert_eq!(link.poll(now + TALLY_HEARTBEAT_ACK_TIMEOUT_MS + 5), None);
    let second = now + 10 + TALLY_HEARTBEAT_INTERVAL_MS;
    assert!(matches!(
        sent(link.poll(second)),
        Some(WireMessage::Heartbeat(_))
    ));

    // No acknowledgement: the transport is presumed dead and closed.
    let dead = second + TALLY_HEARTBEAT_ACK_TIMEOUT_MS;
    assert_eq!(link.poll(dead - 1), None);
    assert_eq!(link.poll(dead), Some(TallyAction::CloseTransport));
    link.transport_closed(dead);
    assert_eq!(
        link.connection(dead),
        TallyConnection::Reconnecting {
            attempt: 1,
            retry_in_ms: TALLY_INITIAL_BACKOFF_MS
        }
    );
    // An offline tally never shows a stale light.
    assert_eq!(indicator(&link, CAMERA, dead), TallyIndicator::Offline);
    assert_eq!(
        link.presentation(&screen(CAMERA), dead)
            .connection
            .status_text(),
        "Connection lost; retry 1 in 0.5 s"
    );
}

#[test]
fn reconnect_backoff_doubles_to_its_cap_and_resets_after_resync() {
    let mut link = link(Role::Viewer);
    assert_eq!(link.start(), Some(TallyAction::OpenTransport));
    assert_eq!(link.start(), None, "start is idempotent");
    assert_eq!(
        link.connection(0),
        TallyConnection::Connecting { attempt: 0 }
    );

    // Refused upgrades (for example a wrong token) look like closed transports.
    let mut now = 0;
    let mut delays = Vec::new();
    for _ in 0..7 {
        link.transport_closed(now);
        let TallyConnection::Reconnecting {
            attempt,
            retry_in_ms,
        } = link.connection(now)
        else {
            panic!("closed transport must schedule a retry");
        };
        delays.push(retry_in_ms);
        assert_eq!(link.poll(now + retry_in_ms - 1), None);
        now += retry_in_ms;
        assert_eq!(link.poll(now), Some(TallyAction::OpenTransport));
        assert_eq!(
            link.connection(now),
            TallyConnection::Connecting { attempt }
        );
    }
    assert_eq!(
        delays,
        [
            500,
            1_000,
            2_000,
            4_000,
            8_000,
            TALLY_MAX_BACKOFF_MS,
            TALLY_MAX_BACKOFF_MS
        ]
    );

    // A stalled handshake is abandoned after the sync timeout.
    link.transport_opened(now).unwrap();
    assert_eq!(
        link.poll(now + TALLY_SYNC_TIMEOUT_MS),
        Some(TallyAction::CloseTransport)
    );
    link.transport_closed(now + TALLY_SYNC_TIMEOUT_MS);
    now += TALLY_SYNC_TIMEOUT_MS + TALLY_MAX_BACKOFF_MS;

    connect(&mut link, Role::Viewer, now, CAMERA, SLIDES);
    // A server shutdown notice ends the session; the backoff starts over.
    let shutdown = on_wire(WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: Some(4),
        error: StructuredError {
            code: "server_shutting_down".to_owned(),
            message: "daemon is stopping".to_owned(),
            fields: Vec::new(),
            retryable: true,
        },
    }));
    assert_eq!(
        link.receive(shutdown, now),
        Some(TallyAction::CloseTransport)
    );
    link.transport_closed(now);
    assert_eq!(
        link.connection(now),
        TallyConnection::Reconnecting {
            attempt: 1,
            retry_in_ms: TALLY_INITIAL_BACKOFF_MS
        }
    );
}

#[test]
fn reconnect_resumes_and_relights_from_the_new_state() {
    let mut link = link(Role::Viewer);
    connect(&mut link, Role::Viewer, 0, CAMERA, SLIDES);
    link.transport_closed(100);
    let retry = 100 + TALLY_INITIAL_BACKOFF_MS;
    assert_eq!(link.poll(retry), Some(TallyAction::OpenTransport));
    let Some(WireMessage::HandshakeRequest(request)) =
        sent(Some(link.transport_opened(retry).unwrap()))
    else {
        panic!("handshake expected");
    };
    let cursor = request
        .resume_cursor
        .expect("reconnect offers a resume cursor");
    assert_eq!(cursor.revision, 4);

    // The server resumes and replays the cut realized while offline.
    let resume = handshake_with(
        CURRENT_PROTOCOL_VERSION,
        Role::Viewer,
        5,
        HandshakeOutcome::Resume { cursor },
    );
    assert_eq!(link.receive(resume, retry), None);
    assert_eq!(link.receive(switcher_event(5, GUEST, CAMERA), retry), None);
    assert_eq!(link.connection(retry), TallyConnection::Live);
    assert_eq!(
        link.receive(realized(5, 1, ManualTransitionStatus::Inactive), retry),
        None
    );
    assert_eq!(
        indicator(&link, GUEST, retry),
        TallyIndicator::Light(TallyLight::Program)
    );
    assert_eq!(
        indicator(&link, CAMERA, retry),
        TallyIndicator::Light(TallyLight::Preview)
    );
}

#[test]
fn non_retryable_rejection_and_protocol_mismatch_stop_reconnecting() {
    let mut rejected = link(Role::Operator);
    rejected.start();
    rejected.transport_opened(0).unwrap();
    let denial = handshake_with(
        CURRENT_PROTOCOL_VERSION,
        Role::Operator,
        0,
        HandshakeOutcome::Rejected {
            error: StructuredError {
                code: "role_denied".to_owned(),
                message: "role is not granted".to_owned(),
                fields: Vec::new(),
                retryable: false,
            },
        },
    );
    assert_eq!(
        rejected.receive(denial, 0),
        Some(TallyAction::CloseTransport)
    );
    rejected.transport_closed(0);
    assert_eq!(
        rejected.connection(60_000),
        TallyConnection::Stopped(TallyStop::Rejected {
            code: "role_denied".to_owned()
        })
    );
    assert_eq!(rejected.poll(60_000), None);

    let mut incompatible = link(Role::Viewer);
    incompatible.start();
    incompatible.transport_opened(0).unwrap();
    let foreign = handshake(Role::Viewer, 0);
    let WireMessage::HandshakeResponse(mut response) = foreign else {
        unreachable!();
    };
    response.protocol = ProtocolVersion::new(
        CURRENT_PROTOCOL_VERSION.major,
        CURRENT_PROTOCOL_VERSION.minor + 1,
    );
    assert_eq!(
        incompatible.receive(WireMessage::HandshakeResponse(response), 0),
        Some(TallyAction::CloseTransport)
    );
    incompatible.transport_closed(0);
    assert_eq!(
        incompatible.connection(0),
        TallyConnection::Stopped(TallyStop::Incompatible)
    );
    assert_eq!(incompatible.poll(60_000), None);

    let mut stopped = link(Role::Viewer);
    connect(&mut stopped, Role::Viewer, 0, CAMERA, SLIDES);
    assert_eq!(stopped.stop(), Some(TallyAction::CloseTransport));
    stopped.transport_closed(0);
    assert_eq!(
        stopped.connection(0),
        TallyConnection::Stopped(TallyStop::Stopped)
    );
    assert_eq!(stopped.poll(60_000), None);
}

#[test]
fn viewer_tally_exposes_only_the_local_full_screen_shortcut() {
    let mut link = link(Role::Viewer);
    connect(&mut link, Role::Viewer, 0, CAMERA, SLIDES);
    let mut screen = screen(GUEST);

    let presentation = link.presentation(&screen, 0);
    assert_eq!(
        presentation.shortcuts,
        [(
            TallyShortcut::ToggleFullScreen,
            TransitionControlState::Enabled
        )]
    );
    assert!(!link.press(TallyShortcut::Cut, &mut screen, "viewer-cut", 0));
    assert!(!link.press(
        TallyShortcut::PreviewSelected,
        &mut screen,
        "viewer-preview",
        0
    ));
    assert_eq!(link.poll(0), None, "no command may be queued for a viewer");

    assert_eq!(
        TallyShortcut::from_key('F'),
        Some(TallyShortcut::ToggleFullScreen)
    );
    assert!(link.press(
        TallyShortcut::from_key('f').unwrap(),
        &mut screen,
        "local",
        0
    ));
    assert!(link.presentation(&screen, 0).full_screen);
    link.transport_closed(1);
    // Full screen stays available offline; it is purely local.
    assert!(link.press(TallyShortcut::ToggleFullScreen, &mut screen, "local", 1));
    assert!(!screen.full_screen);
}

#[test]
fn operator_tally_shortcuts_are_permission_and_state_gated_commands() {
    let mut link = link(Role::Operator);
    connect(&mut link, Role::Operator, 0, CAMERA, SLIDES);
    let mut screen = screen(GUEST);
    assert_eq!(
        link.presentation(&screen, 0).shortcuts,
        [
            (
                TallyShortcut::ToggleFullScreen,
                TransitionControlState::Enabled
            ),
            (
                TallyShortcut::PreviewSelected,
                TransitionControlState::Enabled
            ),
            (TallyShortcut::Cut, TransitionControlState::Enabled),
        ]
    );

    assert!(link.press(
        TallyShortcut::PreviewSelected,
        &mut screen,
        "phone-tally-1:preview:1",
        0
    ));
    let Some(WireMessage::Command(command)) = sent(link.poll(0)) else {
        panic!("preview command queued");
    };
    assert_eq!(
        command.payload,
        CommandPayload::SelectPreview { input: wire(GUEST) }
    );
    assert_eq!(command.idempotency_key, "phone-tally-1:preview:1");
    // Optimistic Preview intent disables a duplicate request but never lights.
    assert_eq!(
        link.shortcut_state(TallyShortcut::PreviewSelected, &screen, 0),
        TransitionControlState::Disabled
    );
    assert_eq!(
        indicator(&link, GUEST, 0),
        TallyIndicator::Light(TallyLight::Idle)
    );

    assert!(link.press(TallyShortcut::Cut, &mut screen, "phone-tally-1:cut:1", 0));
    assert!(matches!(
        sent(link.poll(0)),
        Some(WireMessage::Command(command))
            if command.payload == CommandPayload::Cut
    ));

    // Offline: permissioned shortcuts are hidden because no role is granted.
    link.transport_closed(1);
    assert_eq!(
        link.presentation(&screen, 1).shortcuts,
        [(
            TallyShortcut::ToggleFullScreen,
            TransitionControlState::Enabled
        )]
    );
    assert!(!link.press(TallyShortcut::Cut, &mut screen, "phone-tally-1:cut:2", 1));

    // No selected input: Preview is shown but disabled.
    let mut link = self::link(Role::Operator);
    connect(&mut link, Role::Operator, 0, CAMERA, SLIDES);
    assert_eq!(
        link.shortcut_state(TallyShortcut::PreviewSelected, &TallyScreen::default(), 0),
        TransitionControlState::Disabled
    );
    assert_eq!(
        TallyShortcut::Cut.accessibility_label(),
        "Cut Preview to Program"
    );
}
