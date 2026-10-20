//! Phone and tablet operator surface model: role-gated controls with their
//! disabled reasons, the exact commands the `/operator` page sends, and the
//! Fade-to-Black and stream badges. Every server record is round-tripped
//! through the current wire codec first.

use core::num::NonZeroU128;

use fm_client::Client;
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CapabilityReportSummary, CommandPayload, EngineIdentity,
    FadeToBlackPosition, FadeToBlackState, HandshakeOutcome, HandshakeResponse,
    InputAudioStripStatus, InputStatus, ManualTransitionStatus, OverlayStatus, RecordingStatus,
    Role, ServerIdentity, SnapshotMessage, SnapshotReason, StreamRealizedState, StreamStatus,
    WireInputId, WireMessage, WireRecorderSlot, WireStreamTargetId, decode_line, encode_line,
};
use fm_types::{InputId, ProjectId};
use freemix_web::{
    FadeToBlackPhase, OperatorBlock, OperatorControl, OperatorSurface, OperatorTransition,
    client_config, duration_frames, stream_state_label,
};

const CAMERA: u128 = 21;
const SLIDES: u128 = 22;
const GUEST: u128 = 23;
const STREAM: u128 = 31;

fn wire(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn input(value: u128) -> InputId {
    wire(value).to_domain()
}

fn target(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn project_id() -> ProjectId {
    ProjectId::new(NonZeroU128::new(78).unwrap())
}

#[allow(clippy::needless_pass_by_value)] // takes ownership to read like a send
fn on_wire(message: WireMessage) -> WireMessage {
    decode_line(&encode_line(&message).unwrap()).unwrap()
}

fn permissions(role: Role) -> Vec<String> {
    let names: &[&str] = match role {
        Role::Viewer => &["view_status"],
        Role::Audio => &["view_status", "control_audio"],
        Role::Operator => &[
            "view_status",
            "select_preview",
            "transition",
            "control_audio",
        ],
        _ => unreachable!("tests use viewer, audio, and operator"),
    };
    names.iter().map(|name| (*name).to_owned()).collect()
}

struct Show {
    ftb: FadeToBlackState,
    realized_ftb: FadeToBlackState,
    recording: bool,
    stream_running: bool,
}

impl Default for Show {
    fn default() -> Self {
        let live = FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        };
        Self {
            ftb: live,
            realized_ftb: live,
            recording: false,
            stream_running: false,
        }
    }
}

#[allow(clippy::too_many_lines)] // one flat snapshot fixture
fn snapshot(show: &Show) -> WireMessage {
    on_wire(WireMessage::Snapshot(SnapshotMessage {
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
        engine: EngineIdentity {
            engine_id: "engine-operator".to_owned(),
            state_epoch: 3,
            log_id: "log-operator".to_owned(),
        },
        revision: 9,
        show_name: "Evening Show".to_owned(),
        inputs: [(CAMERA, "Camera 1"), (SLIDES, "Slides"), (GUEST, "Guest")]
            .into_iter()
            .map(|(value, name)| InputStatus {
                input: wire(value),
                name: name.to_owned(),
                lifecycle: fm_protocol::InputLifecycleStatus::default(),
            })
            .collect(),
        outputs: Vec::new(),
        input_audio_strips: vec![
            InputAudioStripStatus {
                input: wire(CAMERA),
                gain_millidb: -6_000,
                balance_basis_points: 250,
                pan_basis_points: -500,
                group: Some(2),
                muted: false,
                soloed: true,
                follow_video: false,
                delay_samples: 480,
            },
            InputAudioStripStatus {
                input: wire(SLIDES),
                gain_millidb: 0,
                balance_basis_points: 0,
                pan_basis_points: 0,
                group: None,
                muted: true,
                soloed: false,
                follow_video: true,
                delay_samples: 0,
            },
            InputAudioStripStatus {
                input: wire(GUEST),
                gain_millidb: 0,
                balance_basis_points: 0,
                pan_basis_points: 0,
                group: None,
                muted: false,
                soloed: false,
                follow_video: true,
                delay_samples: 0,
            },
        ],
        desired_program: wire(CAMERA),
        desired_preview: wire(SLIDES),
        realized_program: wire(CAMERA),
        realized_preview: wire(SLIDES),
        desired_manual_transition: ManualTransitionStatus::Inactive,
        realized_manual_transition: ManualTransitionStatus::Inactive,
        desired_fade_to_black: show.ftb,
        realized_fade_to_black: show.realized_ftb,
        stingers: Vec::new(),
        streams: vec![StreamStatus {
            target: WireStreamTargetId::new(target(STREAM)),
            name: "Main destination".to_owned(),
            desired_running: show.stream_running,
            realized: if show.stream_running {
                StreamRealizedState::Live
            } else {
                StreamRealizedState::Stopped
            },
            detail: None,
        }],
        recordings: [1, 2]
            .into_iter()
            .map(|slot| RecordingStatus {
                recorder: WireRecorderSlot::new(slot).unwrap(),
                desired_active: slot == 1 && show.recording,
            })
            .collect(),
        desired_overlays: OverlayStatus::empty_channels(),
        realized_overlays: OverlayStatus::empty_channels(),
        edit_locks: fm_protocol::EditLockStatus::default(),
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        titles: Vec::new(),
        scenes: Vec::new(),
        layout_scenes: Vec::new(),
        frame_rate: fm_types::FrameRate::new(25, 1).unwrap(),
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
    }))
}

/// A Ready client with the granted `role` and `show` replicated.
fn ready(role: Role, show: &Show) -> Client {
    let mut client = Client::new(client_config(role, "operator-test", project_id())).unwrap();
    client.start_connect().unwrap();
    client.transport_connected().unwrap();
    client
        .intake(on_wire(WireMessage::HandshakeResponse(HandshakeResponse {
            protocol: CURRENT_PROTOCOL_VERSION,
            granted_role: role,
            permissions: permissions(role),
            capabilities: CapabilityReportSummary {
                digest: "sha256:operator".to_owned(),
                total: 1,
                available: 1,
                degraded: 0,
                unavailable: 0,
            },
            server: ServerIdentity {
                engine_id: "engine-operator".to_owned(),
                project_id: project_id().to_string(),
                state_epoch: 3,
                log_id: "log-operator".to_owned(),
            },
            current_revision: 9,
            outcome: HandshakeOutcome::Snapshot {
                reason: SnapshotReason::NoCursor,
            },
        })))
        .unwrap();
    client.intake(snapshot(show)).unwrap();
    assert!(client.model().view().is_some());
    client
}

fn granted(client: &Client) -> Vec<String> {
    client.session().unwrap().permissions.clone()
}

#[test]
fn every_control_states_why_it_is_disabled_for_each_role() {
    let show = Show::default();
    let controls = [
        OperatorControl::Preview(input(GUEST)),
        OperatorControl::Cut,
        OperatorControl::Auto(OperatorTransition::Wipe),
        OperatorControl::FadeToBlack,
        OperatorControl::Recorder(1),
        OperatorControl::Stream(target(STREAM)),
        OperatorControl::Mute(input(CAMERA)),
    ];

    // Not live: every control names the lost session, whatever the role.
    let operator = permissions(Role::Operator);
    let offline = OperatorSurface::new(None, &operator);
    for control in controls {
        assert_eq!(offline.gate(control), Err(OperatorBlock::NotLive));
        assert_eq!(offline.command(control, 25), None);
    }
    assert_eq!(offline.fade_to_black(), None);
    assert_eq!(OperatorBlock::NotLive.reason(), "Not connected to the show");

    // A Viewer sees every control but none works, each naming its permission.
    let viewer = ready(Role::Viewer, &show);
    let permissions = granted(&viewer);
    let view = viewer.model().view();
    let surface = OperatorSurface::new(view.as_ref(), &permissions);
    for control in controls {
        let block = surface.gate(control).unwrap_err();
        assert_eq!(
            block,
            OperatorBlock::MissingPermission(control.required_permission())
        );
        assert_eq!(
            block.reason(),
            format!(
                "This session lacks the {} permission",
                control.required_permission()
            )
        );
    }

    // An Audio session mutes but neither switches nor records.
    let audio = ready(Role::Audio, &show);
    let permissions = granted(&audio);
    let view = audio.model().view();
    let surface = OperatorSurface::new(view.as_ref(), &permissions);
    assert_eq!(surface.gate(OperatorControl::Mute(input(CAMERA))), Ok(()));
    assert_eq!(
        surface.gate(OperatorControl::Cut).unwrap_err().reason(),
        "This session lacks the transition permission"
    );
    assert_eq!(
        surface
            .gate(OperatorControl::Preview(input(GUEST)))
            .unwrap_err()
            .reason(),
        "This session lacks the select_preview permission"
    );

    // An Operator has every control, minus state-specific limits.
    let client = ready(Role::Operator, &show);
    let permissions = granted(&client);
    let view = client.model().view();
    let surface = OperatorSurface::new(view.as_ref(), &permissions);
    for control in controls {
        assert_eq!(surface.gate(control), Ok(()), "{control:?}");
    }
    assert_eq!(
        surface.gate(OperatorControl::Preview(input(SLIDES))),
        Err(OperatorBlock::AlreadyOnPreview)
    );
    assert_eq!(
        OperatorBlock::AlreadyOnPreview.reason(),
        "Already on Preview"
    );
    // The page knows an input added live before its strip replicates.
    assert_eq!(
        OperatorBlock::NoAudioStrip.reason(),
        "Waiting for this input's audio state"
    );
    for control in [
        OperatorControl::Preview(input(99)),
        OperatorControl::Mute(input(99)),
        OperatorControl::Recorder(3),
        OperatorControl::Stream(target(99)),
    ] {
        assert_eq!(surface.gate(control), Err(OperatorBlock::NotInShow));
    }
    assert_eq!(OperatorBlock::NotInShow.reason(), "No longer in the show");
}

#[test]
#[allow(clippy::too_many_lines)] // one walk over every control
fn controls_send_the_exact_wire_commands_the_page_sends() {
    let client = ready(Role::Operator, &Show::default());
    let permissions = granted(&client);
    let view = client.model().view();
    let surface = OperatorSurface::new(view.as_ref(), &permissions);

    assert_eq!(
        surface.command(OperatorControl::Preview(input(GUEST)), 25),
        Some(CommandPayload::SelectPreview { input: wire(GUEST) })
    );
    assert_eq!(
        surface.command(OperatorControl::Cut, 25),
        Some(CommandPayload::Cut)
    );
    let tokens = OperatorTransition::ALL.map(OperatorTransition::payload_token);
    assert_eq!(tokens, ["fade", "alpha_fade", "slide", "zoom", "wipe"]);
    for transition in OperatorTransition::ALL {
        let payload = surface
            .command(OperatorControl::Auto(transition), 13)
            .unwrap();
        // The page builds `payload=<token>\tduration_frames=<n>`.
        let line = encode_line(&WireMessage::Command(fm_protocol::CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: "auto".into(),
            idempotency_key: "auto".into(),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }))
        .unwrap();
        assert!(
            line.contains(&format!(
                "\tpayload={}\tduration_frames=13",
                transition.payload_token()
            )),
            "{line}"
        );
    }
    assert_eq!(
        surface.command(OperatorControl::FadeToBlack, 50),
        Some(CommandPayload::FadeToBlack {
            active: true,
            duration_frames: 50
        })
    );
    assert_eq!(
        surface.command(OperatorControl::Recorder(1), 25),
        Some(CommandPayload::RecordStart {
            recorder: WireRecorderSlot::new(1).unwrap()
        })
    );
    assert_eq!(
        surface.command(OperatorControl::Stream(target(STREAM)), 25),
        Some(CommandPayload::StreamStart {
            target: WireStreamTargetId::new(target(STREAM))
        })
    );
    // A mute flips only `muted`, keeping every other strip value.
    assert_eq!(
        surface.command(OperatorControl::Mute(input(CAMERA)), 25),
        Some(CommandPayload::SetInputAudioStrip {
            input: wire(CAMERA),
            gain_millidb: -6_000,
            balance_basis_points: 250,
            pan_basis_points: -500,
            muted: true,
            soloed: true,
            follow_video: false,
            delay_samples: 480,
        })
    );
    assert!(matches!(
        surface.command(OperatorControl::Mute(input(SLIDES)), 25),
        Some(CommandPayload::SetInputAudioStrip { muted: false, .. })
    ));
    assert_eq!(
        surface.command(OperatorControl::Preview(input(SLIDES)), 25),
        None
    );

    // Toggles stop what runs and lift a fade to black.
    let black = FadeToBlackState {
        target_active: true,
        position: FadeToBlackPosition::BLACK,
    };
    let running = ready(
        Role::Operator,
        &Show {
            ftb: black,
            realized_ftb: black,
            recording: true,
            stream_running: true,
        },
    );
    let view = running.model().view();
    let surface = OperatorSurface::new(view.as_ref(), &permissions);
    assert_eq!(
        surface.command(OperatorControl::FadeToBlack, 25),
        Some(CommandPayload::FadeToBlack {
            active: false,
            duration_frames: 25
        })
    );
    assert_eq!(
        surface.command(OperatorControl::Recorder(1), 25),
        Some(CommandPayload::RecordStop {
            recorder: WireRecorderSlot::new(1).unwrap()
        })
    );
    assert_eq!(
        surface.command(OperatorControl::Recorder(2), 25),
        Some(CommandPayload::RecordStart {
            recorder: WireRecorderSlot::new(2).unwrap()
        })
    );
    assert_eq!(
        surface.command(OperatorControl::Stream(target(STREAM)), 25),
        Some(CommandPayload::StreamStop {
            target: WireStreamTargetId::new(target(STREAM))
        })
    );
    assert_eq!(surface.fade_to_black(), Some(FadeToBlackPhase::Black));
}

#[test]
fn fade_to_black_and_stream_badges_follow_realized_state() {
    let position = FadeToBlackPosition::new;
    for (target_black, numerator, phase, label) in [
        (false, 0, FadeToBlackPhase::Live, "LIVE"),
        (true, 1, FadeToBlackPhase::FadingToBlack, "FADING TO BLACK"),
        (true, 0, FadeToBlackPhase::FadingToBlack, "FADING TO BLACK"),
        (true, u16::MAX, FadeToBlackPhase::Black, "BLACK"),
        (false, 40_000, FadeToBlackPhase::FadingUp, "FADING UP"),
    ] {
        let found = FadeToBlackPhase::from_state(target_black, position(numerator));
        assert_eq!(found, phase);
        assert_eq!(found.label(), label);
    }

    let fading = ready(
        Role::Operator,
        &Show {
            ftb: FadeToBlackState {
                target_active: true,
                position: FadeToBlackPosition::BLACK,
            },
            realized_ftb: FadeToBlackState {
                target_active: true,
                position: position(30_000),
            },
            ..Show::default()
        },
    );
    let permissions = granted(&fading);
    let view = fading.model().view();
    assert_eq!(
        OperatorSurface::new(view.as_ref(), &permissions).fade_to_black(),
        Some(FadeToBlackPhase::FadingToBlack)
    );

    let labels = [
        StreamRealizedState::Stopped,
        StreamRealizedState::Starting,
        StreamRealizedState::Live,
        StreamRealizedState::WaitingToReconnect,
        StreamRealizedState::Congested,
        StreamRealizedState::Failed,
        StreamRealizedState::Unavailable,
    ]
    .map(stream_state_label);
    assert_eq!(
        labels,
        [
            "Stopped",
            "Starting",
            "LIVE",
            "Reconnecting",
            "Congested",
            "Failed",
            "Unavailable"
        ]
    );
}

#[test]
fn durations_round_to_whole_frames_at_the_show_rate() {
    assert_eq!(duration_frames(1_000, 25, 1), 25);
    assert_eq!(duration_frames(500, 25, 1), 13);
    assert_eq!(duration_frames(1_000, 30_000, 1_001), 30);
    assert_eq!(duration_frames(2_000, 60, 1), 120);
    assert_eq!(duration_frames(0, 25, 1), 1);
    assert_eq!(duration_frames(1, 25, 1), 1);
}

fn shortcut(
    id: u64,
    enabled: bool,
    action: fm_protocol::WireShortcutAction,
) -> fm_protocol::ShortcutStatus {
    fm_protocol::ShortcutStatus {
        id,
        spec: fm_protocol::ShortcutSpec {
            name: format!("Shortcut {id}"),
            enabled,
            key: None,
            osc: None,
            action,
        },
    }
}

/// RC-004: a shortcut button needs `transition`, an enabled shortcut, and
/// the permission of every cue action it runs; a cue list GO or a script
/// is left to the daemon's authority. An enabled button sends exactly
/// `shortcut_fire`.
#[test]
fn shortcut_buttons_need_the_permission_of_every_action_they_run() {
    use fm_protocol::{
        WireCueAction, WireOverlayChannelId, WireShortcutAction, WireShortcutCueOperation,
    };
    let show = Show::default();
    let overlay = shortcut(
        1,
        true,
        WireShortcutAction::Actions(vec![WireCueAction::OverlayOn {
            channel: WireOverlayChannelId::new(1).unwrap(),
            input: fm_types::InputTarget::Fixed(wire(CAMERA)),
        }]),
    );
    let mute = shortcut(
        2,
        true,
        WireShortcutAction::Actions(vec![
            WireCueAction::Preview {
                input: fm_types::InputTarget::Fixed(wire(SLIDES)),
            },
            WireCueAction::AudioMute {
                input: fm_types::InputTarget::Fixed(wire(GUEST)),
                muted: true,
            },
        ]),
    );
    let title = shortcut(
        3,
        true,
        WireShortcutAction::Actions(vec![WireCueAction::TitleNumber {
            input: fm_types::InputTarget::Fixed(wire(SLIDES)),
            field: fm_protocol::WireTitleFieldId::new(target(1)),
            add: true,
            value: 1,
        }]),
    );
    let go = shortcut(
        4,
        true,
        WireShortcutAction::Cue {
            list: 1,
            operation: WireShortcutCueOperation::Go { cue: None },
        },
    );
    let off = shortcut(5, false, WireShortcutAction::Script { script: 1 });

    let operator = ready(Role::Operator, &show);
    let granted_operator = granted(&operator);
    let operator_view = operator.model().view();
    let surface = OperatorSurface::new(operator_view.as_ref(), &granted_operator);
    assert_eq!(surface.shortcut_gate(&overlay), Ok(()));
    assert_eq!(
        surface.shortcut_command(&overlay),
        Some(CommandPayload::FireShortcut {
            shortcut: 1,
            targets: fm_protocol::WireTargetBindings::default()
        })
    );
    assert_eq!(surface.shortcut_gate(&mute), Ok(()));
    assert_eq!(surface.shortcut_gate(&go), Ok(()));
    let blocked = surface.shortcut_gate(&title).unwrap_err();
    assert_eq!(blocked, OperatorBlock::MissingPermission("edit_project"));
    assert_eq!(
        blocked.reason(),
        "This session lacks the edit_project permission"
    );
    assert_eq!(surface.shortcut_command(&title), None);
    assert_eq!(
        surface.shortcut_gate(&off),
        Err(OperatorBlock::ShortcutDisabled)
    );
    assert_eq!(
        OperatorBlock::ShortcutDisabled.reason(),
        "This shortcut is disabled"
    );

    // An audio session lacks `transition` for every shortcut.
    let audio = ready(Role::Audio, &show);
    let granted_audio = granted(&audio);
    let audio_view = audio.model().view();
    let surface = OperatorSurface::new(audio_view.as_ref(), &granted_audio);
    for found in [&overlay, &mute, &go] {
        assert_eq!(
            surface.shortcut_gate(found),
            Err(OperatorBlock::MissingPermission("transition"))
        );
    }
    // Without audio control, a shortcut that mutes is held.
    let no_audio: Vec<String> = granted_operator
        .iter()
        .filter(|permission| *permission != "control_audio")
        .cloned()
        .collect();
    let surface = OperatorSurface::new(operator_view.as_ref(), &no_audio);
    assert_eq!(
        surface.shortcut_gate(&mute),
        Err(OperatorBlock::MissingPermission("control_audio"))
    );
    assert_eq!(surface.shortcut_gate(&overlay), Ok(()));
    // Not live.
    let offline = OperatorSurface::new(None, &granted_operator);
    assert_eq!(offline.shortcut_gate(&overlay), Err(OperatorBlock::NotLive));
}
