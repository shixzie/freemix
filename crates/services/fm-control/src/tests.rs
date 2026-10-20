use std::{error::Error, fmt, num::NonZeroU128, sync::mpsc::TryRecvError};

use fm_auth::{Policy, Principal, Role, SessionId, UserId};
use fm_clock::ClockDomainId;
use fm_engine::{Engine, EngineInputAudioStripState, EngineManualTransitionKind, ShowState};
use fm_protocol::{
    CommandMessage, CommandPayload, CommandResult, EngineIdentity, EventCursor, LiveInputSource,
    ManualTransitionKind, ManualTransitionPosition, ManualTransitionStatus, RuntimeLifecycleEvent,
    ServerIdentity, StingerAudioPolicy as ProtocolStingerAudioPolicy,
    StingerMissingMediaFallback as ProtocolStingerFallback, StingerReadiness, StingerStatus,
    WireInputId, WireMessage, WireStingerSlotId,
};
use fm_switcher::{MissingMediaFallback, StingerAudioPolicy, StingerDescriptor, StingerSlotId};
use fm_types::{FrameRate, InputId, OutputId};

use super::*;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn output(value: u128) -> OutputId {
    OutputId::new(NonZeroU128::new(value).unwrap())
}

fn named_inputs(values: impl IntoIterator<Item = InputId>) -> Vec<(InputId, String)> {
    values
        .into_iter()
        .map(|input| (input, format!("Input {input}")))
        .collect()
}

fn principal(role: Role) -> Principal {
    Principal::authenticated(
        UserId::new("user").unwrap(),
        SessionId::new("session").unwrap(),
        [role],
    )
}

fn service(retained_events: usize, subscriber_queue: usize) -> ControlService {
    let show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap();
    let engine = Engine::new(
        show,
        FrameRate::new(60, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(1).unwrap()),
    );
    ControlService::new(
        engine,
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events,
            max_subscribers: 4,
            subscriber_queue,
        },
    )
}

#[test]
fn snapshot_preserves_configured_output_roster_order() {
    let show = ShowState::new(
        "show",
        named_inputs([input(1), input(2)]),
        input(1),
        input(2),
    )
    .unwrap()
    .with_outputs(vec![
        (output(7), "Clean".into()),
        (output(3), "Dirty".into()),
    ])
    .unwrap();
    let control = ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits::default(),
    );
    assert_eq!(
        control
            .snapshot()
            .snapshot
            .outputs
            .iter()
            .map(|output| (output.output.to_domain(), output.name.as_str()))
            .collect::<Vec<_>>(),
        [(output(7), "Clean"), (output(3), "Dirty")]
    );
}

fn stinger_service() -> ControlService {
    let mut show = ShowState::new(
        "stinger show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap();
    let slot = StingerSlotId::new(1).unwrap();
    show.configure_stinger(
        slot,
        StingerDescriptor::new(
            input(3),
            true,
            1,
            StingerAudioPolicy::Muted,
            MissingMediaFallback::KeepProgram,
        ),
    )
    .unwrap();
    show.preload_stinger(slot, true).unwrap();
    ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 8,
            max_subscribers: 4,
            subscriber_queue: 8,
        },
    )
}

#[test]
fn snapshot_projects_canonical_input_names_in_engine_order() {
    let control = service(8, 2);
    assert_eq!(
        control
            .snapshot()
            .snapshot
            .inputs
            .iter()
            .map(|input| (input.input.to_domain(), input.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (input(1), "Input 1"),
            (input(2), "Input 2"),
            (input(3), "Input 3"),
        ]
    );
}

#[test]
fn snapshot_projects_exact_stinger_configuration_and_realized_readiness() {
    let control = stinger_service();
    assert_eq!(
        control.snapshot().snapshot.stingers,
        vec![StingerStatus {
            slot: WireStingerSlotId::new(1).unwrap(),
            media_input: WireInputId::from_domain(input(3)),
            preload: true,
            cut_point_frames: 1,
            audio_policy: ProtocolStingerAudioPolicy::Muted,
            missing_media_fallback: ProtocolStingerFallback::KeepProgram,
            readiness: StingerReadiness::Ready,
        }]
    );
}

#[test]
fn stinger_slot_mutation_is_transition_authorized_and_projects_durable_state() {
    let mut control = service(8, 8);
    let configure = CommandPayload::ConfigureStinger {
        slot: WireStingerSlotId::new(8).unwrap(),
        media_input: WireInputId::from_domain(input(3)),
        preload: true,
        cut_point_frames: 4,
        audio_policy: ProtocolStingerAudioPolicy::MixWithProgram,
        missing_media_fallback: ProtocolStingerFallback::Fade,
    };
    let denied = control
        .submit(
            &principal(Role::Graphics),
            command(
                "configure-denied",
                "configure-denied-key",
                configure.clone(),
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    // The denial carries its typed reason (Protocol 2.45).
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected {
            reason: Some(fm_protocol::RefusalReason::PermissionDenied),
            ..
        }
    ));

    let accepted = control
        .submit(
            &principal(Role::Operator),
            command("configure", "configure-key", configure),
            0,
        )
        .unwrap();
    assert!(matches!(
        accepted.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::StingerSlotsChanged { stingers, .. },
            ..
        }] if stingers.len() == 1 && stingers[0].slot.number() == 8
    ));
    control.tick(&server_identity()).unwrap();
    assert_eq!(
        control.snapshot().snapshot.stingers[0].readiness,
        StingerReadiness::Ready
    );

    control
        .submit(
            &principal(Role::Operator),
            command(
                "remove",
                "remove-key",
                CommandPayload::RemoveStinger {
                    slot: WireStingerSlotId::new(8).unwrap(),
                },
            ),
            0,
        )
        .unwrap();
    control.tick(&server_identity()).unwrap();
    assert_eq!(control.snapshot().snapshot.stingers.len(), 0);
}

fn command(id: &str, key: &str, payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
        id: id.to_owned(),
        idempotency_key: key.to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    }
}

fn prepared<A>(outcome: PrepareSubmitOutcome<'_, A>) -> PreparedSubmission<'_, A> {
    match outcome {
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
        PrepareSubmitOutcome::Replayed(_) => panic!("submission should require a commit"),
    }
}

fn server_identity() -> ServerIdentity {
    ServerIdentity {
        engine_id: "engine-a".to_owned(),
        project_id: "project-a".to_owned(),
        state_epoch: 1,
        log_id: "log-a".to_owned(),
    }
}

#[test]
fn authorization_runs_before_detailed_command_validation() {
    let mut control = service(8, 8);
    let denied = control
        .submit(
            &principal(Role::Viewer),
            command(
                "denied",
                "denied-key",
                CommandPayload::Fade { duration_frames: 0 },
            ),
            0,
        )
        .unwrap();
    let CommandResult::Rejected { code, .. } = denied.output.result else {
        panic!("viewer command should be rejected");
    };
    assert_eq!(code, "permission_denied");

    let invalid = control
        .submit(
            &principal(Role::Operator),
            command(
                "invalid",
                "invalid-key",
                CommandPayload::Fade { duration_frames: 0 },
            ),
            0,
        )
        .unwrap();
    let CommandResult::Rejected { code, .. } = invalid.output.result else {
        panic!("zero fade should be rejected");
    };
    assert_eq!(code, "invalid_command");
}

#[test]
fn manual_transition_is_authorized_durable_reversible_and_replay_safe() {
    let mut control = service(16, 8);
    let denied = control
        .submit(
            &principal(Role::Viewer),
            command(
                "denied-manual",
                "denied-manual-key",
                CommandPayload::StartManualTransition {
                    kind: ManualTransitionKind::Fade,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(control.diagnostics().current_revision, 0);

    let commands = [
        (
            "manual-start",
            CommandPayload::StartManualTransition {
                kind: ManualTransitionKind::Wipe,
            },
        ),
        (
            "manual-forward",
            CommandPayload::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(8_000).unwrap(),
            },
        ),
        (
            "manual-reverse",
            CommandPayload::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(2_500).unwrap(),
            },
        ),
    ];
    for (id, payload) in commands {
        let message = command(id, id, payload);
        let accepted = control
            .submit(&principal(Role::Operator), message.clone(), 0)
            .unwrap();
        assert!(matches!(
            accepted.output.result,
            CommandResult::Accepted { .. }
        ));
        control.tick(&server_identity()).unwrap();
        let replay = control
            .submit(&principal(Role::Operator), message, 0)
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.output.events.len(), 0);
    }
    assert_eq!(
        control
            .engine
            .realized_switcher()
            .t_bar()
            .unwrap()
            .position()
            .basis_points(),
        2_500
    );
    assert_manual_snapshot(&control, 2_500);
    assert!(control.idle_engine_snapshot().is_ok());

    control
        .submit(
            &principal(Role::Operator),
            command(
                "manual-cancel",
                "manual-cancel",
                CommandPayload::CancelManualTransition,
            ),
            0,
        )
        .unwrap();
    control.tick(&server_identity()).unwrap();
    assert!(control.engine.realized_switcher().t_bar().is_none());
    assert_eq!(
        control.snapshot().snapshot.desired_manual_transition,
        ManualTransitionStatus::Inactive
    );
    assert_eq!(
        control.snapshot().snapshot.realized_manual_transition,
        ManualTransitionStatus::Inactive
    );
    assert_eq!(control.engine.realized_switcher().program(), input(1));
    assert_eq!(control.diagnostics().current_revision, 4);
}

#[test]
fn manual_alpha_fade_projects_exact_authoritative_state() {
    let mut control = service(8, 8);
    for (id, payload) in [
        (
            "manual-alpha-start",
            CommandPayload::StartManualTransition {
                kind: ManualTransitionKind::AlphaFade,
            },
        ),
        (
            "manual-alpha-position",
            CommandPayload::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(6_250).unwrap(),
            },
        ),
    ] {
        let submitted = control
            .submit(&principal(Role::Operator), command(id, id, payload), 0)
            .unwrap();
        assert!(matches!(
            submitted.output.result,
            CommandResult::Accepted { .. }
        ));
        control.tick(&server_identity()).unwrap();
    }

    for status in [
        control.snapshot().snapshot.desired_manual_transition,
        control.snapshot().snapshot.realized_manual_transition,
    ] {
        assert!(matches!(
            status,
            ManualTransitionStatus::Active(state)
                if state.kind == ManualTransitionKind::AlphaFade
                    && state.position.basis_points() == 6_250
        ));
    }
    assert_eq!(
        control.engine.realized_manual_transition().unwrap().kind,
        EngineManualTransitionKind::AlphaFade
    );
}

fn assert_manual_snapshot(control: &ControlService<Policy>, position: u16) {
    let desired = control.snapshot().snapshot.desired_manual_transition;
    let realized = control.snapshot().snapshot.realized_manual_transition;
    assert!(matches!(
        desired,
        ManualTransitionStatus::Active(state)
            if state.kind == ManualTransitionKind::Wipe
                && state.interval_start == ManualTransitionPosition::START
                && state.position.basis_points() == position
    ));
    assert!(matches!(
        realized,
        ManualTransitionStatus::Active(state)
            if state.kind == ManualTransitionKind::Wipe
                && state.interval_start.basis_points() == position
                && state.position.basis_points() == position
    ));
}

#[test]
fn wipe_propagates_with_authorization_idempotency_resume_and_exact_endpoints() {
    let mut control = service(8, 8);
    let denied = control
        .submit(
            &principal(Role::Viewer),
            command(
                "denied-wipe",
                "denied-wipe-key",
                CommandPayload::Wipe { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(control.diagnostics().current_revision, 0);

    let identity = control.identity().clone();
    let message = command(
        "wipe",
        "wipe-key",
        CommandPayload::Wipe { duration_frames: 3 },
    );
    let accepted = control
        .submit(&principal(Role::Operator), message.clone(), 0)
        .unwrap();
    assert_eq!(
        accepted.output.result,
        CommandResult::Accepted {
            id: "wipe".to_owned(),
            revision: 1,
            scheduled_frame: Some(0),
        }
    );
    assert_eq!(accepted.output.events.len(), 1);

    let duplicate = control
        .submit(&principal(Role::Operator), message, 0)
        .unwrap();
    assert!(duplicate.replayed);
    assert_eq!(duplicate.output.result, accepted.output.result);
    assert_eq!(duplicate.output.events.len(), 0);
    assert_eq!(control.diagnostics().current_revision, 1);

    let ResumeDecision::Events(events) = control.resume(&EventCursor {
        engine: identity,
        revision: 0,
    }) else {
        panic!("the accepted wipe should be resumable");
    };
    assert_eq!(events, accepted.output.events);

    for (start, end) in [(0, 1), (1, 2), (2, 3)] {
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(
            tick.frame
                .program
                .transition_kind
                .map(|kind| format!("{kind:?}")),
            Some("Wipe".to_owned())
        );
        assert_eq!(
            (
                tick.frame.program.mix_start_numerator,
                tick.frame.program.mix_end_numerator,
            ),
            (start, end)
        );
        assert_eq!(tick.runtime_events.len(), usize::from(end == 3));
    }

    let endpoint = control.tick(&server_identity()).unwrap();
    assert_eq!(endpoint.frame.program.primary, input(2));
    assert_eq!(endpoint.frame.program.secondary, None);
    assert_eq!(endpoint.frame.program.transition_kind, None);
    assert_eq!(control.diagnostics().current_revision, 1);
}

#[test]
fn alpha_fade_propagates_with_exact_runtime_kind_and_endpoints() {
    let mut control = service(8, 8);
    let accepted = control
        .submit(
            &principal(Role::Operator),
            command(
                "alpha-fade",
                "alpha-fade-key",
                CommandPayload::AlphaFade { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        accepted.output.result,
        CommandResult::Accepted {
            revision: 1,
            scheduled_frame: Some(0),
            ..
        }
    ));

    for (start, end) in [(0, 1), (1, 2), (2, 3)] {
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(
            tick.frame
                .program
                .transition_kind
                .map(|kind| format!("{kind:?}")),
            Some("AlphaFade".to_owned())
        );
        assert_eq!(
            (
                tick.frame.program.mix_start_numerator,
                tick.frame.program.mix_end_numerator,
            ),
            (start, end)
        );
        assert_eq!(tick.runtime_events.len(), usize::from(end == 3));
    }

    let endpoint = control.tick(&server_identity()).unwrap();
    assert_eq!(endpoint.frame.program.primary, input(2));
    assert_eq!(endpoint.frame.program.secondary, None);
    assert_eq!(endpoint.frame.program.transition_kind, None);
}

#[test]
fn slide_propagates_with_exact_runtime_kind_and_endpoints() {
    let mut control = service(8, 8);
    let accepted = control
        .submit(
            &principal(Role::Operator),
            command(
                "slide",
                "slide-key",
                CommandPayload::Slide { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        accepted.output.result,
        CommandResult::Accepted {
            revision: 1,
            scheduled_frame: Some(0),
            ..
        }
    ));

    for (start, end) in [(0, 1), (1, 2), (2, 3)] {
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(
            tick.frame
                .program
                .transition_kind
                .map(|kind| format!("{kind:?}")),
            Some("Slide".to_owned())
        );
        assert_eq!(
            (
                tick.frame.program.mix_start_numerator,
                tick.frame.program.mix_end_numerator,
            ),
            (start, end)
        );
        assert_eq!(tick.runtime_events.len(), usize::from(end == 3));
    }

    let endpoint = control.tick(&server_identity()).unwrap();
    assert_eq!(endpoint.frame.program.primary, input(2));
    assert_eq!(endpoint.frame.program.secondary, None);
    assert_eq!(endpoint.frame.program.transition_kind, None);
}

#[test]
fn zoom_propagates_with_exact_runtime_kind_and_endpoints() {
    let mut control = service(8, 8);
    let accepted = control
        .submit(
            &principal(Role::Operator),
            command(
                "zoom",
                "zoom-key",
                CommandPayload::Zoom { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        accepted.output.result,
        CommandResult::Accepted {
            revision: 1,
            scheduled_frame: Some(0),
            ..
        }
    ));

    for (start, end) in [(0, 1), (1, 2), (2, 3)] {
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(
            tick.frame
                .program
                .transition_kind
                .map(|kind| format!("{kind:?}")),
            Some("Zoom".to_owned())
        );
        assert_eq!(
            (
                tick.frame.program.mix_start_numerator,
                tick.frame.program.mix_end_numerator,
            ),
            (start, end)
        );
        assert_eq!(tick.runtime_events.len(), usize::from(end == 3));
    }

    let endpoint = control.tick(&server_identity()).unwrap();
    assert_eq!(endpoint.frame.program.primary, input(2));
    assert_eq!(endpoint.frame.program.secondary, None);
    assert_eq!(endpoint.frame.program.transition_kind, None);
}

#[test]
fn stinger_slot_propagates_with_exact_runtime_kind_and_endpoints() {
    let mut control = stinger_service();
    let accepted = control
        .submit(
            &principal(Role::Operator),
            command(
                "stinger",
                "stinger-key",
                CommandPayload::Stinger {
                    slot: WireStingerSlotId::new(1).unwrap(),
                    duration_frames: 3,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        accepted.output.result,
        CommandResult::Accepted {
            revision: 1,
            scheduled_frame: Some(0),
            ..
        }
    ));

    for (start, end) in [(0, 1), (1, 2), (2, 3)] {
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(
            tick.frame.program.transition_kind,
            Some(fm_switcher::TransitionKind::Stinger(
                StingerSlotId::new(1).unwrap()
            ))
        );
        assert_eq!(
            (
                tick.frame.program.mix_start_numerator,
                tick.frame.program.mix_end_numerator,
            ),
            (start, end)
        );
    }
    assert_eq!(
        control
            .tick(&server_identity())
            .unwrap()
            .frame
            .program
            .primary,
        input(2)
    );
}

#[test]
fn fade_to_black_is_authorized_reversible_and_runtime_ordered_with_program() {
    let mut control = service(16, 8);
    let operator = principal(Role::Operator);
    let server = server_identity();

    let denied = control
        .submit(
            &principal(Role::Viewer),
            command(
                "denied-ftb",
                "denied-ftb-key",
                CommandPayload::FadeToBlack {
                    active: true,
                    duration_frames: 4,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));

    let accepted = control
        .submit(
            &operator,
            command(
                "ftb-on",
                "ftb-on-key",
                CommandPayload::FadeToBlack {
                    active: true,
                    duration_frames: 4,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        accepted.output.events[0].payload,
        EventPayload::DesiredSwitcher {
            fade_to_black: FadeToBlackState {
                target_active: true,
                position: FadeToBlackPosition::BLACK,
            },
            ..
        }
    ));
    assert_eq!(
        control.snapshot().snapshot.realized_fade_to_black.position,
        FadeToBlackPosition::LIVE
    );

    let first = control.tick(&server).unwrap();
    assert_eq!(first.runtime_events.len(), 0);
    assert_eq!(first.frame.fade_to_black.interval_end().numerator(), 16_383);

    control
        .submit(
            &operator,
            command("cut-during-ftb", "cut-during-ftb-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    let cut = control.tick(&server).unwrap();
    assert_eq!(cut.runtime_events.len(), 1);
    assert_eq!(cut.runtime_events[0].revision, 2);
    assert_eq!(cut.runtime_events[0].generation, 2);
    assert_eq!(cut.runtime_events[0].sequence, 1);
    assert_eq!(control.tick(&server).unwrap().runtime_events.len(), 0);

    let completed = control.tick(&server).unwrap();
    assert_eq!(completed.runtime_events.len(), 1);
    assert_eq!(completed.runtime_events[0].revision, 1);
    assert_eq!(completed.runtime_events[0].generation, 2);
    assert_eq!(completed.runtime_events[0].sequence, 2);
    assert!(matches!(
        completed.runtime_events[0].event,
        RuntimeLifecycleEvent::Realized {
            fade_to_black: FadeToBlackState {
                target_active: true,
                position: FadeToBlackPosition::BLACK,
            },
            ..
        }
    ));
    assert!(control.idle_engine_snapshot().is_ok());
}

#[test]
fn reversing_fade_to_black_supersedes_only_the_displaced_intent() {
    let mut control = service(16, 8);
    let operator = principal(Role::Operator);
    let server = server_identity();
    control
        .submit(
            &operator,
            command(
                "ftb-on",
                "ftb-on-key",
                CommandPayload::FadeToBlack {
                    active: true,
                    duration_frames: 1,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(control.tick(&server).unwrap().runtime_events.len(), 1);
    control
        .submit(
            &operator,
            command(
                "ftb-off",
                "ftb-off-key",
                CommandPayload::FadeToBlack {
                    active: false,
                    duration_frames: 3,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(control.tick(&server).unwrap().runtime_events.len(), 0);
    control
        .submit(
            &operator,
            command(
                "ftb-reverse",
                "ftb-reverse-key",
                CommandPayload::FadeToBlack {
                    active: true,
                    duration_frames: 3,
                },
            ),
            0,
        )
        .unwrap();
    let reversed = control.tick(&server).unwrap();
    assert!(matches!(
        reversed.runtime_events.as_slice(),
        [RuntimeEventMessage {
            revision: 2,
            generation: 3,
            sequence: 1,
            event: RuntimeLifecycleEvent::Superseded { by_revision: 3 },
            ..
        }]
    ));
    assert_eq!(control.tick(&server).unwrap().runtime_events.len(), 0);
    let reversed_completion = control.tick(&server).unwrap();
    assert!(matches!(
        reversed_completion.runtime_events.as_slice(),
        [RuntimeEventMessage {
            revision: 3,
            generation: 3,
            sequence: 2,
            event: RuntimeLifecycleEvent::Realized {
                fade_to_black: FadeToBlackState {
                    target_active: true,
                    position: FadeToBlackPosition::BLACK,
                },
                ..
            },
            ..
        }]
    ));
}

#[test]
fn accepted_preparation_is_isolated_until_commit_and_projects_exactly() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    let before_diagnostics = control.diagnostics();
    let before_snapshot = control.snapshot().clone();
    let before_engine = control.engine.snapshot().unwrap();
    let message = command("prepared-cut", "prepared-cut-key", CommandPayload::Cut);

    let first = prepared(
        control
            .prepare_submit(&principal(Role::Operator), message.clone(), 0)
            .unwrap(),
    );
    assert!(first.submission().is_accepted());
    assert_eq!(first.idempotency_key().as_str(), "prepared-cut-key");
    assert_eq!(first.output().events.len(), 1);
    let first_output = first.output().clone();
    let projected = first.project(1).unwrap();
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    first.abort();

    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(control.engine.snapshot().unwrap(), before_engine);
    assert!(control.log.is_empty());
    assert!(control.pending_runtime_actions.is_empty());
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));

    let second = prepared(
        control
            .prepare_submit(&principal(Role::Operator), message.clone(), 0)
            .unwrap(),
    );
    let prospective = second.submission().clone();
    assert_eq!(second.project(1).unwrap(), projected);
    let committed = second.commit().unwrap();

    assert_eq!(committed.output, first_output);
    assert_eq!(committed, prospective);
    assert_eq!(control.diagnostics().current_revision, 1);
    assert_eq!(control.log.len(), 1);
    assert_eq!(control.pending_runtime_actions.len(), 1);
    assert_eq!(control.snapshot().snapshot.revision, 1);
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(ref event) if event == &committed.output.events[0]
    ));

    control.tick(&server_identity()).unwrap();
    assert_eq!(control.engine.snapshot().unwrap(), projected);

    let replay = control
        .prepare_submit(&principal(Role::Operator), message, 0)
        .unwrap();
    let PrepareSubmitOutcome::Replayed(replay) = replay else {
        panic!("durable duplicate must not create a prepared guard");
    };
    assert!(replay.replayed);
    assert!(replay.accepted.is_none());
    assert_eq!(replay.output.events.len(), 0);
    assert_eq!(replay.output.result, committed.output.result);
}

#[test]
fn rejected_preparation_and_drop_have_no_effects_but_commit_installs_receipt() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    let before_diagnostics = control.diagnostics();
    let before_snapshot = control.snapshot().clone();
    let before_engine = control.engine.snapshot().unwrap();
    let message = command(
        "prepared-rejection",
        "prepared-rejection-key",
        CommandPayload::SelectPreview {
            input: WireInputId::from_domain(input(99)),
        },
    );

    let rejected = prepared(
        control
            .prepare_submit(&principal(Role::Operator), message.clone(), 0)
            .unwrap(),
    );
    assert!(matches!(
        rejected.output().result,
        CommandResult::Rejected { ref code, .. } if code == "not_found"
    ));
    assert_eq!(rejected.output().events.len(), 0);
    assert_eq!(rejected.project(0).unwrap().receipts().len(), 1);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    drop(rejected);

    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(control.engine.snapshot().unwrap(), before_engine);
    assert!(control.log.is_empty());

    let rejected = prepared(
        control
            .prepare_submit(&principal(Role::Operator), message.clone(), 0)
            .unwrap(),
    );
    let prospective = rejected.submission().clone();
    let projected = rejected.project(0).unwrap();
    let committed = rejected.commit().unwrap();
    assert_eq!(committed, prospective);
    assert_eq!(control.engine.snapshot().unwrap(), projected);
    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert!(control.log.is_empty());
    assert!(control.pending_runtime_actions.is_empty());
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));

    assert!(matches!(
        control
            .prepare_submit(&principal(Role::Operator), message, 0)
            .unwrap(),
        PrepareSubmitOutcome::Replayed(_)
    ));
}

#[test]
fn authorization_denial_is_staged_and_cached_only_by_commit() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    let before_diagnostics = control.diagnostics();
    let before_snapshot = control.snapshot().clone();
    let before_engine = control.engine.snapshot().unwrap();
    let message = command(
        "prepared-denial",
        "prepared-denial-key",
        CommandPayload::Cut,
    );
    let key = IdempotencyKey::new("prepared-denial-key");

    let denied = prepared(
        control
            .prepare_submit(&principal(Role::Viewer), message.clone(), 0)
            .unwrap(),
    );
    assert!(matches!(
        denied.output().result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(denied.project(0).unwrap(), before_engine);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    denied.abort();

    assert!(!control.authorization_denials.contains_key(&key));
    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(control.engine.snapshot().unwrap(), before_engine);

    let denied = prepared(
        control
            .prepare_submit(&principal(Role::Viewer), message, 0)
            .unwrap(),
    );
    let prospective = denied.submission().clone();
    let committed = denied.commit().unwrap();
    assert_eq!(committed, prospective);
    assert_eq!(
        control.authorization_denials.get(&key),
        Some(&committed.output.result)
    );
    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(control.engine.snapshot().unwrap(), before_engine);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));

    let replayed = control
        .prepare_submit(
            &principal(Role::Operator),
            command("different", "prepared-denial-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    let PrepareSubmitOutcome::Replayed(replayed) = replayed else {
        panic!("committed denial must replay without a guard");
    };
    assert!(replayed.replayed);
    assert_eq!(replayed.output.result, committed.output.result);
}

#[test]
fn authorization_denial_replays_after_role_and_command_change_without_engine_mutation() {
    let mut control = service(8, 8);
    let before = control.snapshot().clone();
    let denied = control
        .submit(
            &principal(Role::Viewer),
            command(
                "denied",
                "denied-key",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(99)),
                },
            ),
            0,
        )
        .unwrap();

    let replayed = control
        .submit(
            &principal(Role::Operator),
            command("different-id", "denied-key", CommandPayload::Cut),
            0,
        )
        .unwrap();

    assert_eq!(replayed.output.result, denied.output.result);
    assert!(replayed.replayed);
    assert_eq!(replayed.output.events.len(), 0);
    assert!(replayed.accepted.is_none());
    assert_eq!(control.snapshot(), &before);
    assert_eq!(control.diagnostics().current_revision, 0);
}

#[test]
fn authorization_denial_does_not_replace_an_existing_engine_receipt() {
    let mut control = service(8, 8);
    let accepted = control
        .submit(
            &principal(Role::Operator),
            command("cut", "shared-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(accepted.is_accepted());

    let denied = control
        .submit(
            &principal(Role::Viewer),
            command("viewer-cut", "shared-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));

    let retried = control
        .submit(
            &principal(Role::Operator),
            command("cut", "shared-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(retried.replayed);
    assert_eq!(retried.output.result, accepted.output.result);
}

#[test]
fn authorization_denial_cache_uses_the_retained_event_bound() {
    let mut control = service(2, 8);
    for key in ["one", "two", "three"] {
        control
            .submit(
                &principal(Role::Viewer),
                command(key, key, CommandPayload::Cut),
                0,
            )
            .unwrap();
    }

    assert_eq!(control.authorization_denials.len(), 2);
    assert!(
        !control
            .authorization_denials
            .contains_key(&IdempotencyKey::new("one"))
    );
}

#[test]
fn engine_controls_accepted_rejected_and_duplicate_behavior() {
    let mut control = service(8, 8);
    let operator = principal(Role::Operator);
    let accepted = control
        .submit(
            &operator,
            command(
                "select",
                "select-key",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(3)),
                },
            ),
            0,
        )
        .unwrap();
    assert!(accepted.is_accepted());
    assert!(!accepted.replayed);
    assert!(accepted.accepted.is_some());
    assert_eq!(accepted.output.events.len(), 1);

    let mut duplicate_message = command("other-id", "select-key", CommandPayload::Cut);
    duplicate_message.expected_revision = Some(99);
    let duplicate = control.submit(&operator, duplicate_message, 0).unwrap();
    assert!(duplicate.is_accepted());
    assert!(duplicate.replayed);
    assert!(duplicate.accepted.is_none());
    assert_eq!(duplicate.output.events.len(), 0);
    assert_eq!(duplicate.output.result, accepted.output.result);

    let rejected = control
        .submit(
            &operator,
            command(
                "unknown",
                "unknown-key",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(99)),
                },
            ),
            0,
        )
        .unwrap();
    let CommandResult::Rejected {
        code,
        current_revision,
        reason,
        ..
    } = rejected.output.result
    else {
        panic!("unknown input should be rejected");
    };
    assert_eq!(code, "not_found");
    // The engine's typed reason reaches the wire result (Protocol 2.45).
    assert_eq!(
        reason,
        Some(fm_protocol::RefusalReason::UnknownId {
            kind: fm_protocol::IdKind::Input,
            id: 99,
        })
    );
    assert_eq!(current_revision, 1);
    assert_eq!(control.diagnostics().current_revision, 1);
}

#[test]
fn restored_engine_rejection_receipts_still_replay_after_authorization() {
    let show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap();
    let mut engine = Engine::new(
        show,
        FrameRate::new(60, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(1).unwrap()),
    );
    let mut original_message = command("original", "restored-key", CommandPayload::Cut);
    original_message.expected_revision = Some(1);
    let original = engine
        .execute(original_message.domain_envelope(EngineCommand::Cut), 0)
        .unwrap();
    let restored = Engine::restore(engine.snapshot().unwrap()).unwrap();
    let mut control = ControlService::new(
        restored,
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits::default(),
    );

    let replayed = control
        .submit(
            &principal(Role::Operator),
            command(
                "different",
                "restored-key",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(99)),
                },
            ),
            0,
        )
        .unwrap();

    assert!(replayed.replayed);
    assert_eq!(replayed.output.result, command_result(&original.receipt));
    assert_eq!(control.diagnostics().current_revision, 0);
}

#[test]
fn command_result_is_represented_before_its_events() {
    let mut control = service(8, 8);
    let submission = control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    let messages = submission.output.into_wire_messages();
    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0], WireMessage::CommandResult(_)));
    let WireMessage::Event(event) = &messages[1] else {
        panic!("the durable event should follow the result");
    };
    assert_eq!(event.cursor.revision, 1);
    assert_eq!(
        event.payload,
        EventPayload::DesiredSwitcher {
            program: WireInputId::from_domain(input(2)),
            preview: WireInputId::from_domain(input(1)),
            manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            overlays: fm_protocol::OverlayStatus::empty_channels(),
            input_audio_strips: [input(1), input(2), input(3)]
                .into_iter()
                .map(|input| fm_protocol::InputAudioStripStatus {
                    pan_basis_points: 0,
                    group: None,
                    input: WireInputId::from_domain(input),
                    gain_millidb: 0,
                    balance_basis_points: 0,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                    delay_samples: 0,
                })
                .collect(),
        }
    );
}

#[test]
fn input_audio_strip_flows_through_authorization_snapshot_event_and_frame() {
    let mut control = service(8, 8);
    let server = server_identity();
    let submission = control
        .submit(
            &principal(Role::Operator),
            command(
                "audio-strip",
                "audio-strip-key",
                CommandPayload::SetInputAudioStrip {
                    pan_basis_points: 0,
                    input: WireInputId::from_domain(input(2)),
                    gain_millidb: -6_000,
                    balance_basis_points: 2_500,
                    muted: true,
                    soloed: true,
                    follow_video: false,
                    delay_samples: 2_400,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        submission.output.result,
        CommandResult::Accepted { .. }
    ));
    let EventPayload::DesiredSwitcher {
        input_audio_strips, ..
    } = &submission.output.events[0].payload
    else {
        panic!("expected desired-state event")
    };
    assert!(input_audio_strips.iter().any(|status| {
        status.input == WireInputId::from_domain(input(2))
            && status.gain_millidb == -6_000
            && status.balance_basis_points == 2_500
            && status.muted
            && !status.follow_video
            && status.delay_samples == 2_400
    }));
    assert!(
        control
            .snapshot()
            .snapshot
            .input_audio_strips
            .iter()
            .any(|status| status.input == WireInputId::from_domain(input(2))
                && status.delay_samples == 2_400)
    );

    let tick = control.tick(&server).unwrap();
    assert_eq!(
        tick.frame.input_audio_strip_updates,
        [(
            input(2),
            EngineInputAudioStripState {
                pan_basis_points: 0,
                gain_millidb: -6_000,
                balance_basis_points: 2_500,
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 2_400,
            }
        )]
    );
}

#[test]
fn resume_returns_all_contiguous_retained_revisions() {
    let mut control = service(8, 8);
    let operator = principal(Role::Operator);
    let identity = control.identity().clone();
    control
        .submit(
            &operator,
            command(
                "select",
                "key-1",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(3)),
                },
            ),
            0,
        )
        .unwrap();
    control
        .submit(&operator, command("cut", "key-2", CommandPayload::Cut), 0)
        .unwrap();

    let ResumeDecision::Events(events) = control.resume(&EventCursor {
        engine: identity,
        revision: 0,
    }) else {
        panic!("retained cursor should resume");
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.cursor.revision)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn wrong_identity_and_compacted_cursor_fall_back_to_snapshot() {
    let mut control = service(2, 8);
    let operator = principal(Role::Operator);
    let identity = control.identity().clone();
    for (id, key, target) in [
        ("one", "key-1", 3),
        ("two", "key-2", 2),
        ("three", "key-3", 3),
    ] {
        control
            .submit(
                &operator,
                command(
                    id,
                    key,
                    CommandPayload::SelectPreview {
                        input: WireInputId::from_domain(input(target)),
                    },
                ),
                0,
            )
            .unwrap();
    }

    assert!(matches!(
        control.resume(&EventCursor {
            engine: identity.clone(),
            revision: 0,
        }),
        ResumeDecision::Snapshot(_)
    ));
    let wrong_identity = EngineIdentity {
        log_id: "other-log".to_owned(),
        ..identity
    };
    assert!(matches!(
        control.resume(&EventCursor {
            engine: wrong_identity,
            revision: 2,
        }),
        ResumeDecision::Snapshot(_)
    ));
}

#[test]
fn retained_log_never_exceeds_its_bound() {
    let mut control = service(2, 8);
    let operator = principal(Role::Operator);
    for (id, target) in [("one", 3), ("two", 2), ("three", 3)] {
        control
            .submit(
                &operator,
                command(
                    id,
                    id,
                    CommandPayload::SelectPreview {
                        input: WireInputId::from_domain(input(target)),
                    },
                ),
                0,
            )
            .unwrap();
    }
    let diagnostics = control.diagnostics();
    assert_eq!(diagnostics.oldest_retained_revision, Some(2));
    assert_eq!(diagnostics.newest_retained_revision, Some(3));
}

#[test]
fn slow_subscriber_is_failed_and_removed() {
    let mut control = service(8, 1);
    let subscription = control.subscribe().unwrap();
    let operator = principal(Role::Operator);
    control
        .submit(
            &operator,
            command(
                "one",
                "key-1",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(3)),
                },
            ),
            0,
        )
        .unwrap();
    let second = control
        .submit(
            &operator,
            command(
                "two",
                "key-2",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(2)),
                },
            ),
            0,
        )
        .unwrap();

    assert_eq!(
        second.subscriber_failures,
        [SubscriberFailure {
            subscriber: subscription.id(),
            reason: SubscriberFailureReason::SlowClient,
        }]
    );
    assert_eq!(
        subscription.failure(),
        Some(SubscriberFailureReason::SlowClient)
    );
    assert_eq!(control.diagnostics().subscriber_count, 0);
}

#[test]
fn unsubscribe_releases_idle_subscriber_capacity() {
    let mut control = service(8, 8);
    let mut subscriptions: Vec<_> = (0..4).map(|_| control.subscribe().unwrap()).collect();
    let idle = subscriptions.pop().unwrap();

    assert!(matches!(
        control.subscribe(),
        Err(SubscribeError::LimitReached)
    ));
    assert!(control.unsubscribe(idle.id()));
    assert_eq!(idle.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(idle.failure(), None);
    assert_eq!(control.diagnostics().subscriber_count, 3);
    control.subscribe().unwrap();
    assert_eq!(control.diagnostics().subscriber_count, 4);
}

#[test]
fn subscription_distinguishes_durable_and_runtime_events() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(EventMessage { .. })
    ));

    control.tick(&server_identity()).unwrap();
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Runtime(RuntimeEventMessage { .. })
    ));
}

#[test]
fn ticks_do_not_change_durable_resume_history() {
    let mut control = service(8, 8);
    let identity = control.identity().clone();
    control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    control.tick(&server_identity()).unwrap();
    control.tick(&server_identity()).unwrap();

    let ResumeDecision::Events(events) = control.resume(&EventCursor {
        engine: identity,
        revision: 0,
    }) else {
        panic!("ticks must not create a durable resume gap");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cursor.revision, 1);
    assert_eq!(control.diagnostics().current_revision, 1);
}

/// A speculative copy executes commands (with their receipts and ticks)
/// without the service or its subscribers seeing any of it: dropping the
/// copy leaves nothing behind, and adopting it installs its state and only
/// then publishes what it held back, in order.
#[test]
fn a_speculation_publishes_nothing_until_it_is_adopted() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    let operator = principal(Role::Operator);
    let server = server_identity();
    let cut = || command("cut", "cut-key", CommandPayload::Cut);

    let mut abandoned = control.speculate();
    abandoned.submit(&operator, cut(), 0).unwrap();
    abandoned.tick(&server).unwrap();
    assert_eq!(abandoned.diagnostics().current_revision, 1);
    drop(abandoned);
    assert!(matches!(subscription.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(control.engine.frame_cursor().get(), 0);

    let mut adopted = control.speculate();
    adopted.submit(&operator, cut(), 0).unwrap();
    adopted.tick(&server).unwrap();
    adopted
        .submit(
            &operator,
            command(
                "preview",
                "preview-key",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(3)),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(subscription.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(control.adopt(adopted), []);
    assert_eq!(control.diagnostics().current_revision, 2);
    let published: Vec<_> = std::iter::from_fn(|| subscription.try_recv().ok()).collect();
    assert!(
        matches!(
            published.as_slice(),
            [
                LiveEvent::Durable(EventMessage { cursor: first, .. }),
                LiveEvent::Runtime(_),
                ..,
                LiveEvent::Durable(EventMessage { cursor: last, .. }),
            ] if first.revision == 1 && last.revision == 2
        ),
        "{published:?}"
    );
    // The adopted service publishes as it goes again, and the cut's receipt
    // came with it.
    control.tick(&server).unwrap();
    assert!(subscription.try_recv().is_ok());
    let replayed = control.submit(&operator, cut(), 0).unwrap();
    assert!(replayed.replayed);
}

#[test]
fn runtime_ordering_is_independent_per_generation() {
    let mut control = service(8, 8);
    let operator = principal(Role::Operator);
    let server = server_identity();
    control
        .submit(&operator, command("cut", "cut-key", CommandPayload::Cut), 0)
        .unwrap();
    let first = control.tick(&server).unwrap();
    control
        .submit(
            &operator,
            command(
                "preview",
                "preview-key",
                CommandPayload::SelectPreview {
                    input: WireInputId::from_domain(input(3)),
                },
            ),
            0,
        )
        .unwrap();
    let second = control.tick(&server).unwrap();

    assert_eq!(first.runtime_events[0].revision, 1);
    assert_eq!(first.runtime_events[0].generation, 1);
    assert_eq!(first.runtime_events[0].sequence, 1);
    assert_eq!(second.runtime_events[0].revision, 2);
    assert_eq!(second.runtime_events[0].generation, 2);
    assert_eq!(second.runtime_events[0].sequence, 1);
    assert_eq!(first.runtime_events[0].server, server);
    assert!(matches!(
        second.runtime_events[0].event,
        RuntimeLifecycleEvent::Realized { ref domain, .. } if domain == "switcher"
    ));
}

#[test]
fn tick_realizes_the_command_on_its_exact_scheduled_frame() {
    let mut control = service(8, 8);
    assert_eq!(
        control.next_frame_deadline().unwrap(),
        fm_clock::ClockTime::ZERO
    );
    let submission = control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    let CommandResult::Accepted {
        scheduled_frame, ..
    } = submission.output.result
    else {
        panic!("cut should be accepted");
    };

    let projected = control.project_next_frame().unwrap();
    assert_eq!(projected.frame.get(), scheduled_frame.unwrap());
    assert_eq!(projected.program.primary, input(2));
    assert_eq!(
        control.next_frame_deadline().unwrap(),
        fm_clock::ClockTime::ZERO
    );

    let tick = control.tick(&server_identity()).unwrap();
    assert_eq!(tick.frame, projected);
    assert_eq!(scheduled_frame, Some(tick.frame.frame.get()));
    assert_eq!(tick.frame.frame.get(), 0);
    assert_eq!(tick.frame.program.primary, input(2));
    assert_eq!(tick.frame.runtime_generation.get(), 1);
    assert_eq!(
        control.next_frame_deadline().unwrap(),
        fm_clock::ClockTime::from_nanos(16_666_666)
    );
    assert_eq!(tick.runtime_events.len(), 1);
    assert_eq!(
        control.snapshot().snapshot.realized_program.to_domain(),
        input(2)
    );
}

#[test]
fn shutdown_ticks_settle_fade_without_claiming_runtime_realization() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    control
        .submit(
            &principal(Role::Operator),
            command(
                "fade",
                "fade-key",
                CommandPayload::Fade { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(_)
    ));

    for _ in 0..3 {
        let outcome = control.tick_for_shutdown(&server_identity()).unwrap();
        assert_eq!(outcome.runtime_events.len(), 0);
        assert_eq!(outcome.subscriber_failures.len(), 0);
        assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    }

    assert!(control.idle_engine_snapshot().is_ok());
    assert_eq!(control.runtime_sequence, 0);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RealizerFailure;

impl fmt::Display for RealizerFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("render output failed")
    }
}

impl Error for RealizerFailure {}

#[test]
fn successful_realizer_observes_exact_frame_before_publication() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(_)
    ));

    let mut projected = control.engine.clone();
    let expected = projected.tick().unwrap();
    let outcome = control
        .tick_with_realizer(&server_identity(), |frame| {
            assert_eq!(frame, &expected);
            assert_eq!(frame.program, expected.program);
            assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
            Ok::<(), RealizerFailure>(())
        })
        .unwrap();

    assert_eq!(outcome.frame, expected);
    assert_eq!(outcome.runtime_events.len(), 1);
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Runtime(ref event) if event == &outcome.runtime_events[0]
    ));
}

#[test]
fn failed_realizer_is_fatal_without_false_publication_or_control_updates() {
    let mut control = service(8, 8);
    let subscription = control.subscribe().unwrap();
    control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(_)
    ));

    let before_snapshot = control.snapshot().clone();
    let before_pending = control.pending_runtime_actions.clone();
    let before_active_transitions = control.active_transitions.clone();
    let before_runtime_generation = control.runtime_sequence_generation;
    let before_runtime_sequence = control.runtime_sequence;
    let mut projected = control.engine.clone();
    let expected = projected.tick().unwrap();

    let error = control
        .tick_with_realizer(&server_identity(), |frame| {
            assert_eq!(frame, &expected);
            Err(RealizerFailure)
        })
        .unwrap_err();

    assert_eq!(error.tick_error(), None);
    assert_eq!(error.realization_error(), Some(&RealizerFailure));
    assert_eq!(
        error.to_string(),
        "frame realization failed: render output failed"
    );
    assert!(error.source().is_some());
    assert_eq!(error.into_realization_error(), Some(RealizerFailure));
    assert_eq!(
        control.engine.runtime_generation(),
        expected.runtime_generation
    );
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(control.pending_runtime_actions, before_pending);
    assert_eq!(control.active_transitions, before_active_transitions);
    assert_eq!(
        control.runtime_sequence_generation,
        before_runtime_generation
    );
    assert_eq!(control.runtime_sequence, before_runtime_sequence);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(
        control.idle_engine_snapshot(),
        Err(SnapshotError::WorkInFlight)
    );
}

#[test]
fn idle_engine_snapshot_preserves_engine_idle_checks() {
    let mut control = service(8, 8);
    assert_eq!(
        control.idle_engine_snapshot().unwrap(),
        control.engine.snapshot().unwrap()
    );

    control
        .submit(
            &principal(Role::Operator),
            command("cut", "cut-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
    assert_eq!(
        control.idle_engine_snapshot(),
        Err(SnapshotError::WorkInFlight)
    );
}

fn streamed_service() -> ControlService {
    let show = ShowState::new(
        "stream show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap()
    .with_streams([
        (
            fm_switcher::StreamTargetId::new(5).unwrap(),
            "Twitch".into(),
        ),
        (
            fm_switcher::StreamTargetId::new(9).unwrap(),
            "YouTube".into(),
        ),
    ])
    .unwrap();
    ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits::default(),
    )
}

fn wire_target(value: u128) -> fm_protocol::WireStreamTargetId {
    fm_protocol::WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
}

fn stream_status(target: u128, name: &str, desired_running: bool) -> StreamStatus {
    StreamStatus {
        target: wire_target(target),
        name: name.to_owned(),
        desired_running,
        realized: StreamRealizedState::Stopped,
        detail: None,
    }
}

#[test]
fn stream_commands_require_transition_permission_and_project_full_status() {
    let mut control = streamed_service();
    let denied = control
        .submit(
            &principal(Role::Viewer),
            command(
                "denied",
                "denied-key",
                CommandPayload::StreamStart {
                    target: wire_target(9),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(
        control.snapshot().snapshot.streams,
        vec![
            stream_status(5, "Twitch", false),
            stream_status(9, "YouTube", false),
        ]
    );

    let accepted = control
        .submit(
            &principal(Role::Operator),
            command(
                "stream-on",
                "stream-on-key",
                CommandPayload::StreamStart {
                    target: wire_target(9),
                },
            ),
            0,
        )
        .unwrap();
    assert!(accepted.is_accepted());
    assert_eq!(
        accepted.output.events,
        vec![EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: control.identity().clone(),
                revision: 1,
            },
            payload: EventPayload::StreamsChanged {
                streams: vec![
                    stream_status(5, "Twitch", false),
                    stream_status(9, "YouTube", true),
                ],
            },
        }]
    );

    let replay = control
        .submit(
            &principal(Role::Operator),
            command(
                "again",
                "stream-on-key",
                CommandPayload::StreamStart {
                    target: wire_target(9),
                },
            ),
            0,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.output.events.len(), 0);
}

#[test]
fn admin_can_stop_streams_and_snapshots_carry_the_desired_projection() {
    let mut control = streamed_service();
    control
        .submit(
            &principal(Role::Operator),
            command(
                "on",
                "on-key",
                CommandPayload::StreamStart {
                    target: wire_target(5),
                },
            ),
            0,
        )
        .unwrap();
    control.tick(&server_identity()).unwrap();
    assert_eq!(
        control.snapshot().snapshot.streams,
        vec![
            stream_status(5, "Twitch", true),
            stream_status(9, "YouTube", false),
        ]
    );

    let stopped = control
        .submit(
            &principal(Role::Admin),
            command(
                "off",
                "off-key",
                CommandPayload::StreamStop {
                    target: wire_target(5),
                },
            ),
            0,
        )
        .unwrap();
    assert!(stopped.is_accepted());
    assert_eq!(
        stopped.output.events[0].payload,
        EventPayload::StreamsChanged {
            streams: vec![
                stream_status(5, "Twitch", false),
                stream_status(9, "YouTube", false),
            ],
        }
    );
    control.tick(&server_identity()).unwrap();
    assert!(!control.snapshot().snapshot.streams[0].desired_running);
}

fn recorder_slot(value: u8) -> WireRecorderSlot {
    WireRecorderSlot::new(value).unwrap()
}

fn recording_status(recorder: u8, desired_active: bool) -> RecordingStatus {
    RecordingStatus {
        recorder: recorder_slot(recorder),
        desired_active,
    }
}

fn record_command(id: &str, key: &str, recorder: u8, start: bool) -> CommandMessage {
    let recorder = recorder_slot(recorder);
    command(
        id,
        key,
        if start {
            CommandPayload::RecordStart { recorder }
        } else {
            CommandPayload::RecordStop { recorder }
        },
    )
}

#[test]
fn record_commands_require_transition_permission_and_project_both_slots() {
    let mut control = streamed_service();
    let denied = control
        .submit(
            &principal(Role::Viewer),
            record_command("denied", "denied-key", 1, true),
            0,
        )
        .unwrap();
    assert!(matches!(
        denied.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(
        control.snapshot().snapshot.recordings,
        [recording_status(1, false), recording_status(2, false)]
    );

    let accepted = control
        .submit(
            &principal(Role::Operator),
            record_command("rec-one-on", "rec-one-on-key", 1, true),
            0,
        )
        .unwrap();
    assert!(accepted.is_accepted());
    assert_eq!(
        accepted.output.events,
        vec![EventMessage {
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            cursor: EventCursor {
                engine: control.identity().clone(),
                revision: 1,
            },
            payload: EventPayload::RecordingChanged {
                recorder: recorder_slot(1),
                active: true,
            },
        }]
    );
    assert_eq!(
        control.snapshot().snapshot.recordings,
        [recording_status(1, true), recording_status(2, false)]
    );

    let second = control
        .submit(
            &principal(Role::Operator),
            record_command("rec-two-on", "rec-two-on-key", 2, true),
            0,
        )
        .unwrap();
    assert_eq!(
        second.output.events[0].payload,
        EventPayload::RecordingChanged {
            recorder: recorder_slot(2),
            active: true,
        }
    );
    assert_eq!(
        control.snapshot().snapshot.recordings,
        [recording_status(1, true), recording_status(2, true)]
    );

    let viewer_denied_stop = control
        .submit(
            &principal(Role::Viewer),
            record_command("viewer-off", "viewer-off-key", 2, false),
            0,
        )
        .unwrap();
    assert!(matches!(
        viewer_denied_stop.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
}

#[test]
fn record_command_replays_and_stops_only_the_addressed_slot() {
    let mut control = streamed_service();
    let accepted = control
        .submit(
            &principal(Role::Operator),
            record_command("rec-one-on", "rec-one-key", 1, true),
            0,
        )
        .unwrap();
    assert!(accepted.is_accepted());
    let second = control
        .submit(
            &principal(Role::Operator),
            record_command("rec-two-on", "rec-two-key", 2, true),
            0,
        )
        .unwrap();
    assert!(second.is_accepted());

    let replay = control
        .submit(
            &principal(Role::Operator),
            record_command("again", "rec-one-key", 1, true),
            0,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.output.events.len(), 0);

    let stopped = control
        .submit(
            &principal(Role::Operator),
            record_command("rec-one-off", "rec-one-off-key", 1, false),
            0,
        )
        .unwrap();
    assert!(stopped.is_accepted());
    assert_eq!(
        stopped.output.events[0].payload,
        EventPayload::RecordingChanged {
            recorder: recorder_slot(1),
            active: false,
        }
    );
    control.tick(&server_identity()).unwrap();
    assert_eq!(
        control.snapshot().snapshot.recordings,
        [recording_status(1, false), recording_status(2, true)]
    );
}

fn rename(id: &str, input_value: u128, name: &str) -> CommandMessage {
    command(
        id,
        &format!("{id}-key"),
        CommandPayload::RenameInput {
            input: WireInputId::from_domain(input(input_value)),
            name: name.to_owned(),
        },
    )
}

fn undo(id: &str, expected_edit: Option<u64>) -> CommandMessage {
    command(
        id,
        &format!("{id}-key"),
        CommandPayload::Undo { expected_edit },
    )
}

fn redo(id: &str, expected_edit: Option<u64>) -> CommandMessage {
    command(
        id,
        &format!("{id}-key"),
        CommandPayload::Redo { expected_edit },
    )
}

fn lock(id: &str, scope: fm_protocol::EditLockScope, locked: bool) -> CommandMessage {
    command(
        id,
        &format!("{id}-key"),
        CommandPayload::SetEditLock { scope, locked },
    )
}

fn rejection_field_code(submission: &CommandSubmission) -> Option<&str> {
    match &submission.output.result {
        CommandResult::Rejected { fields, .. } => fields.first().map(|field| field.code.as_str()),
        CommandResult::Accepted { .. } => None,
    }
}

fn rejection_code(submission: &CommandSubmission) -> Option<&str> {
    match &submission.output.result {
        CommandResult::Rejected { code, .. } => Some(code.as_str()),
        CommandResult::Accepted { .. } => None,
    }
}

fn assert_wire_exact(control: &ControlService, submission: &CommandSubmission) {
    for message in submission.output.clone().into_wire_messages() {
        let line = fm_protocol::encode_line(&message).unwrap();
        assert_eq!(fm_protocol::decode_line(&line).unwrap(), message);
    }
    let snapshot = WireMessage::Snapshot(control.snapshot().snapshot.clone());
    let line = fm_protocol::encode_line(&snapshot).unwrap();
    assert_eq!(fm_protocol::decode_line(&line).unwrap(), snapshot);
}

#[test]
fn history_and_padlock_commands_follow_the_edit_project_authorization_matrix() {
    let roles = [
        (Role::Viewer, false),
        (Role::Graphics, true),
        (Role::Audio, false),
        (Role::Operator, false),
        (Role::Admin, true),
    ];
    let payloads = [
        CommandPayload::Undo {
            expected_edit: None,
        },
        CommandPayload::Redo {
            expected_edit: None,
        },
        CommandPayload::SetEditLock {
            scope: fm_protocol::EditLockScope::Project,
            locked: true,
        },
        CommandPayload::SetEditLock {
            scope: fm_protocol::EditLockScope::Input(WireInputId::from_domain(input(2))),
            locked: true,
        },
    ];
    for (role, permitted) in roles {
        for payload in &payloads {
            let mut control = service(8, 8);
            // Give undo something to revert so a permitted undo is accepted.
            assert!(
                control
                    .submit(&principal(Role::Admin), rename("seed", 1, "Seed"), 0)
                    .unwrap()
                    .is_accepted()
            );
            let submission = control
                .submit(
                    &principal(role),
                    command("probe", "probe-key", payload.clone()),
                    0,
                )
                .unwrap();
            let denied = rejection_code(&submission) == Some("permission_denied");
            assert_eq!(
                denied, !permitted,
                "{role:?} {payload:?}: {:?}",
                submission.output.result
            );
            if permitted && !matches!(payload, CommandPayload::Redo { .. }) {
                assert!(submission.is_accepted(), "{role:?} {payload:?}");
            }
            if denied {
                // A denial is target-free and changes nothing.
                assert_eq!(control.snapshot().snapshot.revision, 1);
                assert_eq!(control.snapshot().snapshot.edit_history.undo_depth, 1);
            }
        }
    }
}

#[test]
fn undo_of_a_transition_class_edit_also_requires_the_transition_permission() {
    let mut control = service(8, 8);
    let configure = CommandPayload::ConfigureStinger {
        slot: WireStingerSlotId::new(2).unwrap(),
        media_input: WireInputId::from_domain(input(3)),
        preload: false,
        cut_point_frames: 6,
        audio_policy: ProtocolStingerAudioPolicy::StingerOnly,
        missing_media_fallback: ProtocolStingerFallback::Cut,
    };
    assert!(
        control
            .submit(
                &principal(Role::Operator),
                command("configure", "configure-key", configure),
                0
            )
            .unwrap()
            .is_accepted()
    );
    control.tick(&server_identity()).unwrap();
    // Transports authorize exactly what prepare_submit authorizes.
    let undo_payload = CommandPayload::Undo {
        expected_edit: None,
    };
    let required = control.required_classes(&undo_payload);
    assert_eq!(required.primary(), CommandClass::EditProject);
    assert_eq!(required.additional(), [CommandClass::Transition]);
    assert!(
        required
            .authorize(&Policy::production(), &principal(Role::Graphics))
            .is_err()
    );
    assert!(
        required
            .authorize(&Policy::production(), &principal(Role::Admin))
            .is_ok()
    );
    // Nothing to redo yet: redo needs only its own class.
    let redo_payload = CommandPayload::Redo {
        expected_edit: None,
    };
    assert_eq!(control.required_classes(&redo_payload).additional(), []);

    // Graphics may edit the project but may not change Stinger slots, so it
    // may not revert a Stinger slot change through history either.
    let denied = control
        .submit(&principal(Role::Graphics), undo("graphics-undo", None), 0)
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    // Operator may change Stinger slots but may not use project history.
    let denied = control
        .submit(&principal(Role::Operator), undo("operator-undo", None), 0)
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    assert_eq!(control.snapshot().snapshot.stingers.len(), 1);

    let undone = control
        .submit(&principal(Role::Admin), undo("admin-undo", Some(1)), 0)
        .unwrap();
    assert!(undone.is_accepted());
    // Redo reapplies the Stinger change, so it needs Transition as well.
    assert_eq!(
        control.required_classes(&redo_payload).additional(),
        [CommandClass::Transition]
    );
    assert_eq!(control.required_classes(&undo_payload).additional(), []);
    assert!(matches!(
        undone.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::StingerSlotsChanged { stingers, .. },
            edit_history: EditHistoryStatus {
                undo_depth: 0,
                next_undo: None,
                redo_depth: 1,
                next_redo: Some(1),
            },
            ..
        }] if stingers.is_empty()
    ));
    assert_wire_exact(&control, &undone);
}

#[test]
fn undo_and_redo_project_history_events_snapshots_and_a_replayable_command() {
    let mut control = service(8, 8);
    let admin = principal(Role::Admin);
    let renamed = control
        .submit(&admin, rename("rename", 1, "Host"), 0)
        .unwrap();
    assert!(matches!(
        renamed.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputRenamed { name, .. },
            edit_history: EditHistoryStatus {
                undo_depth: 1,
                next_undo: Some(1),
                redo_depth: 0,
                next_redo: None,
            },
            ..
        }] if name == "Host"
    ));
    control.tick(&server_identity()).unwrap();

    // A live operation leaves the history unchanged but still carries it.
    let cut = control
        .submit(&admin, command("cut", "cut-key", CommandPayload::Cut), 0)
        .unwrap();
    assert_eq!(
        cut.output.events[0].edit_history,
        EditHistoryStatus {
            undo_depth: 1,
            next_undo: Some(1),
            redo_depth: 0,
            next_redo: None,
        }
    );
    control.tick(&server_identity()).unwrap();

    // Naming another edit is a conflict, not a silent revert.
    let conflict = control
        .submit(&admin, undo("undo-stale", Some(2)), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&conflict), Some("history_conflict"));

    // The journal records the resolved edit, never the history command.
    let message = undo("undo", Some(1));
    let prepared = prepared(control.prepare_submit(&admin, message.clone(), 0).unwrap());
    let replay = prepared.replay_command(&message);
    assert_eq!(
        replay,
        CommandMessage {
            payload: CommandPayload::RenameInput {
                input: WireInputId::from_domain(input(1)),
                name: "Input 1".to_owned(),
            },
            ..message.clone()
        }
    );
    let undone = prepared.commit().unwrap();
    assert!(matches!(
        undone.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputRenamed { name, .. },
            edit_history: EditHistoryStatus {
                undo_depth: 0,
                next_undo: None,
                redo_depth: 1,
                next_redo: Some(1),
            },
            ..
        }] if name == "Input 1"
    ));
    assert_wire_exact(&control, &undone);
    // The undo realizes in the project domain like the edit it resolved to.
    let tick = control.tick(&server_identity()).unwrap();
    assert!(matches!(
        tick.runtime_events.as_slice(),
        [RuntimeEventMessage {
            revision: 3,
            event: RuntimeLifecycleEvent::Realized { domain, .. },
            ..
        }] if domain == "project"
    ));
    assert_eq!(control.snapshot().snapshot.edit_history.redo_depth, 1);

    let nothing = control.submit(&admin, undo("undo-again", None), 0).unwrap();
    assert_eq!(rejection_field_code(&nothing), Some("nothing_to_undo"));
    // A rejected undo is journaled as itself.
    let again = undo("undo-third", None);
    let rejected = prepared_or_replayed_replay(&mut control, &admin, &again);
    assert_eq!(rejected, again);

    let redone = control.submit(&admin, redo("redo", Some(1)), 0).unwrap();
    assert!(matches!(
        redone.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputRenamed { name, .. },
            edit_history: EditHistoryStatus {
                undo_depth: 1,
                next_undo: Some(1),
                redo_depth: 0,
                next_redo: None,
            },
            ..
        }] if name == "Host"
    ));
    assert_wire_exact(&control, &redone);
    let snapshot = &control.snapshot().snapshot;
    assert_eq!(snapshot.inputs[0].name, "Host");
    assert_eq!(snapshot.edit_history.next_undo, Some(1));
}

fn prepared_or_replayed_replay(
    control: &mut ControlService,
    principal: &Principal,
    message: &CommandMessage,
) -> CommandMessage {
    let prepared = prepared(
        control
            .prepare_submit(principal, message.clone(), 0)
            .unwrap(),
    );
    let replay = prepared.replay_command(message);
    prepared.abort();
    replay
}

#[test]
fn padlock_blocks_project_edits_and_history_but_never_live_switching() {
    let mut control = service(8, 8);
    let admin = principal(Role::Admin);
    let graphics = principal(Role::Graphics);
    assert!(
        control
            .submit(&admin, rename("rename", 1, "Host"), 0)
            .unwrap()
            .is_accepted()
    );
    control.tick(&server_identity()).unwrap();

    let locked = control
        .submit(
            &graphics,
            lock("lock", fm_protocol::EditLockScope::Project, true),
            0,
        )
        .unwrap();
    assert!(matches!(
        locked.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::EditLockChanged {
                scope: fm_protocol::EditLockScope::Project,
                locked: true,
            },
            edit_history: EditHistoryStatus { undo_depth: 1, .. },
            ..
        }]
    ));
    assert_wire_exact(&control, &locked);
    let tick = control.tick(&server_identity()).unwrap();
    assert!(matches!(
        tick.runtime_events.as_slice(),
        [RuntimeEventMessage {
            event: RuntimeLifecycleEvent::Realized { domain, .. },
            ..
        }] if domain == "project"
    ));
    assert!(control.snapshot().snapshot.edit_locks.project);

    let refused = control
        .submit(&admin, rename("locked-rename", 2, "Guest"), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("project_locked"));
    let refused = control
        .submit(&admin, undo("locked-undo", None), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("project_locked"));

    let cut = control
        .submit(&admin, command("cut", "cut-key", CommandPayload::Cut), 0)
        .unwrap();
    assert!(cut.is_accepted());
    control.tick(&server_identity()).unwrap();

    // Swap the project padlock for one input's padlock.
    let input_two = fm_protocol::EditLockScope::Input(WireInputId::from_domain(input(2)));
    let input_locked = control
        .submit(&graphics, lock("lock-two", input_two, true), 0)
        .unwrap();
    assert_wire_exact(&control, &input_locked);
    control.tick(&server_identity()).unwrap();
    let unlocked = control
        .submit(
            &graphics,
            lock("unlock", fm_protocol::EditLockScope::Project, false),
            0,
        )
        .unwrap();
    assert!(unlocked.is_accepted());
    control.tick(&server_identity()).unwrap();
    let snapshot = &control.snapshot().snapshot;
    assert_eq!(
        snapshot.edit_locks,
        EditLockStatus {
            project: false,
            inputs: vec![WireInputId::from_domain(input(2))],
        }
    );
    let refused = control
        .submit(&admin, rename("rename-two", 2, "Guest"), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("input_locked"));
    assert!(
        control
            .submit(&admin, rename("rename-three", 3, "Wide"), 0)
            .unwrap()
            .is_accepted()
    );
    // Locking an input outside the show is refused by the engine.
    let unknown = control
        .submit(
            &graphics,
            lock(
                "lock-unknown",
                fm_protocol::EditLockScope::Input(WireInputId::from_domain(input(9))),
                true,
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));
}

#[test]
fn wire_edit_payload_round_trips_every_undoable_engine_edit() {
    use fm_switcher::{OverlayBorderPreset, OverlayPositionPreset, OverlayTransitionKind};
    let overlay = OverlayChannelId::new(3).unwrap();
    let edits = [
        EngineCommand::RenameInput {
            input: input(2),
            name: "Two".to_owned(),
        },
        EngineCommand::ReorderInputs {
            inputs: vec![input(3), input(1), input(2)],
        },
        EngineCommand::ConfigureStinger {
            slot: StingerSlotId::new(4).unwrap(),
            descriptor: StingerDescriptor::new(
                input(3),
                true,
                9,
                StingerAudioPolicy::MixWithProgram,
                MissingMediaFallback::Fade,
            ),
        },
        EngineCommand::RemoveStinger {
            slot: StingerSlotId::new(4).unwrap(),
        },
        EngineCommand::ConfigureOverlayTransition {
            channel: overlay,
            transition: OverlayTransitionKind::Fade,
            duration_frames: 12,
        },
        EngineCommand::ConfigureOverlayAppearance {
            channel: overlay,
            position: OverlayPositionPreset::BottomRight,
            border: OverlayBorderPreset::ThickWhite,
        },
        EngineCommand::SetOverlayOutputInclusion {
            channel: overlay,
            output: output(5),
            included: true,
        },
        EngineCommand::AddInput {
            input: input(7),
            name: "Seven".to_owned(),
            source: fm_engine::EngineInputSource::Solid {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 4,
            },
        },
        EngineCommand::AddInput {
            input: input(8),
            name: "Eight".to_owned(),
            source: fm_engine::EngineInputSource::Bars,
        },
        // Removal is a project edit; it is never recorded, but undo of an
        // add resolves to it and the journal must carry it.
        EngineCommand::RemoveInput { input: input(7) },
    ];
    for edit in edits {
        assert_eq!(edit.kind(), fm_engine::EngineCommandKind::ProjectEdit);
        let payload = wire_edit_payload(&edit).unwrap();
        assert_eq!(engine_command(&payload), edit);
    }
    assert_eq!(wire_edit_payload(&EngineCommand::Cut), None);
    assert_eq!(
        usize::from(fm_protocol::MAX_EDIT_HISTORY_DEPTH),
        fm_engine::MAX_EDIT_HISTORY
    );
}

fn add_input(id: &str, input_value: u128, name: &str) -> CommandMessage {
    command(
        id,
        &format!("{id}-key"),
        CommandPayload::AddInput {
            input: WireInputId::from_domain(input(input_value)),
            name: name.to_owned(),
            source: LiveInputSource::Solid {
                red: 0,
                green: 128,
                blue: 255,
                alpha: 255,
            },
        },
    )
}

fn remove_input(id: &str, input_value: u128) -> CommandMessage {
    command(
        id,
        &format!("{id}-key"),
        CommandPayload::RemoveInput {
            input: WireInputId::from_domain(input(input_value)),
        },
    )
}

#[test]
fn input_add_and_remove_follow_the_edit_project_authorization_matrix() {
    let roles = [
        (Role::Viewer, false),
        (Role::Graphics, true),
        (Role::Audio, false),
        (Role::Operator, false),
        (Role::Admin, true),
    ];
    for (role, permitted) in roles {
        for probe in [add_input("probe", 4, "Four"), remove_input("probe", 3)] {
            let mut control = service(8, 8);
            let submission = control.submit(&principal(role), probe.clone(), 0).unwrap();
            let denied = rejection_code(&submission) == Some("permission_denied");
            assert_eq!(
                denied, !permitted,
                "{role:?} {:?}: {:?}",
                probe.payload, submission.output.result
            );
            assert_eq!(submission.is_accepted(), permitted, "{role:?}");
            let snapshot = &control.snapshot().snapshot;
            let expected_inputs = match (&probe.payload, permitted) {
                (CommandPayload::AddInput { .. }, true) => 4,
                (CommandPayload::RemoveInput { .. }, true) => 2,
                _ => 3,
            };
            assert_eq!(snapshot.inputs.len(), expected_inputs, "{role:?}");
            assert_eq!(snapshot.input_audio_strips.len(), expected_inputs);
        }
    }
}

#[test]
fn input_add_and_remove_project_events_snapshots_history_and_journal() {
    let mut control = service(8, 8);
    let graphics = principal(Role::Graphics);
    let added = control
        .submit(&graphics, add_input("add", 4, "Slate"), 0)
        .unwrap();
    assert!(matches!(
        added.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputAdded { input: added, name, network_stream: None, capture: None, system_audio: None, still: None, screen_capture: None },
            edit_history: EditHistoryStatus {
                undo_depth: 1,
                next_undo: Some(1),
                redo_depth: 0,
                next_redo: None,
            },
            ..
        }] if added.to_domain() == input(4) && name == "Slate"
    ));
    assert_wire_exact(&control, &added);
    let tick = control.tick(&server_identity()).unwrap();
    assert!(matches!(
        tick.runtime_events.as_slice(),
        [RuntimeEventMessage {
            event: RuntimeLifecycleEvent::Realized { domain, .. },
            ..
        }] if domain == "project"
    ));
    let snapshot = &control.snapshot().snapshot;
    assert_eq!(
        snapshot.inputs.last(),
        Some(&InputStatus {
            input: WireInputId::from_domain(input(4)),
            name: "Slate".to_owned(),
            lifecycle: fm_protocol::InputLifecycleStatus::default(),
        })
    );
    assert!(
        snapshot
            .input_audio_strips
            .iter()
            .any(|strip| strip.input.to_domain() == input(4) && strip.follow_video)
    );

    // Undo of the add is journaled as the removal it resolved to.
    let message = undo("undo", Some(1));
    let staged = prepared(
        control
            .prepare_submit(&graphics, message.clone(), 0)
            .unwrap(),
    );
    assert_eq!(
        staged.replay_command(&message).payload,
        CommandPayload::RemoveInput {
            input: WireInputId::from_domain(input(4)),
        }
    );
    let undone = staged.commit().unwrap();
    assert!(matches!(
        undone.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputRemoved { input: removed },
            edit_history: EditHistoryStatus { undo_depth: 0, redo_depth: 1, .. },
            ..
        }] if removed.to_domain() == input(4)
    ));
    assert_wire_exact(&control, &undone);
    control.tick(&server_identity()).unwrap();

    // Redo is journaled as the exact add, source included.
    let message = redo("redo", Some(1));
    let staged = prepared(
        control
            .prepare_submit(&graphics, message.clone(), 0)
            .unwrap(),
    );
    assert_eq!(
        staged.replay_command(&message).payload,
        add_input("unused", 4, "Slate").payload
    );
    assert!(staged.commit().unwrap().is_accepted());
    control.tick(&server_identity()).unwrap();
    assert_eq!(control.snapshot().snapshot.inputs.len(), 4);

    // Removal is not undoable: the history keeps only the add, which it
    // forgets because the add names the removed input.
    let removed = control
        .submit(&graphics, remove_input("remove", 4), 0)
        .unwrap();
    assert!(matches!(
        removed.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputRemoved { .. },
            edit_history: EditHistoryStatus::EMPTY,
            ..
        }]
    ));
    assert_wire_exact(&control, &removed);
    control.tick(&server_identity()).unwrap();
    assert_eq!(control.snapshot().snapshot.inputs.len(), 3);
}

#[test]
fn input_add_and_remove_respect_the_padlock_and_live_references() {
    let mut control = service(8, 8);
    let graphics = principal(Role::Graphics);
    let on_program = control
        .submit(&graphics, remove_input("program", 1), 0)
        .unwrap();
    assert_eq!(rejection_code(&on_program), Some("conflict"));
    assert_eq!(rejection_field_code(&on_program), Some("input_in_use"));

    assert!(
        control
            .submit(
                &graphics,
                lock(
                    "lock-three",
                    fm_protocol::EditLockScope::Input(WireInputId::from_domain(input(3))),
                    true,
                ),
                0,
            )
            .unwrap()
            .is_accepted()
    );
    control.tick(&server_identity()).unwrap();
    let locked = control
        .submit(&graphics, remove_input("locked", 3), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&locked), Some("input_locked"));
    // An input padlock never blocks adding another input.
    assert!(
        control
            .submit(&graphics, add_input("add", 4, "Four"), 0)
            .unwrap()
            .is_accepted()
    );
    control.tick(&server_identity()).unwrap();

    assert!(
        control
            .submit(
                &graphics,
                lock("lock", fm_protocol::EditLockScope::Project, true),
                0,
            )
            .unwrap()
            .is_accepted()
    );
    control.tick(&server_identity()).unwrap();
    for refused in [
        add_input("add-locked", 5, "Five"),
        remove_input("remove-locked", 4),
    ] {
        let submission = control.submit(&graphics, refused, 0).unwrap();
        assert_eq!(rejection_field_code(&submission), Some("project_locked"));
    }
    assert_eq!(control.snapshot().snapshot.inputs.len(), 4);
}

mod titles {
    use fm_protocol::{
        OverlayStatus, TitleAirRequirement, TitleFieldStatus, TitleFieldUpdate, TitleFieldValue,
        TitleStateMessage, TitleSummary, WireOverlayChannelId, WireTitleFieldId,
    };
    use fm_titles::{FieldDefinition, TemplateId, TitleFieldSet, TitleTemplate};

    use super::*;

    const TITLE: u128 = 3;
    const NAME: u128 = 1;
    const SCORE: u128 = 2;

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn template() -> TitleTemplate {
        TitleTemplate {
            id: TemplateId::new(NonZeroU128::new(9).unwrap()),
            name: "Lower third".into(),
            width: 64,
            height: 16,
            background: TitleColor::TRANSPARENT,
            fields: vec![
                FieldDefinition {
                    id: FieldId::new(NonZeroU128::new(NAME).unwrap()),
                    name: "Name".into(),
                    default: FieldValue::Text("Ana".into()),
                    exposed: true,
                },
                FieldDefinition {
                    id: FieldId::new(NonZeroU128::new(SCORE).unwrap()),
                    name: "Score".into(),
                    default: FieldValue::Number(0),
                    exposed: true,
                },
            ],
            elements: Vec::new(),
            animation: fm_titles::TitleAnimation::default(),
        }
    }

    fn title_service() -> ControlService {
        let mut show = ShowState::new(
            "show",
            named_inputs([input(1), input(2), input(TITLE)]),
            input(1),
            input(2),
        )
        .unwrap();
        show.set_title(
            input(TITLE),
            EngineTitle::new(
                "Lower third",
                TitleFieldSet::from_template(&template()).unwrap(),
            ),
        )
        .unwrap();
        let engine = Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        );
        ControlService::new(
            engine,
            Policy::production(),
            "engine-a",
            "log-a",
            ControlLimits {
                retained_events: 8,
                max_subscribers: 4,
                subscriber_queue: 8,
            },
        )
    }

    /// [`title_service`] whose template has a 10-frame In and a 5-frame Out
    /// phase at 30/1: 20 and 10 frames of the 60/1 engine.
    fn animated_title_service() -> ControlService {
        use fm_titles::fm_animation::Phase;

        let mut template = template();
        template
            .edit_animation(|clip| {
                clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
                clip.set_phase_frames_keeping_keys(Phase::Out, 5)
            })
            .unwrap();
        let mut show = ShowState::new(
            "show",
            named_inputs([input(1), input(2), input(TITLE)]),
            input(1),
            input(2),
        )
        .unwrap();
        show.set_title(
            input(TITLE),
            EngineTitle::new(
                "Lower third",
                TitleFieldSet::from_template(&template).unwrap(),
            )
            .with_timing(fm_titles::TitleAirTiming::of(&template.animation)),
        )
        .unwrap();
        let engine = Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        );
        ControlService::new(
            engine,
            Policy::production(),
            "engine-a",
            "log-a",
            ControlLimits {
                retained_events: 8,
                max_subscribers: 4,
                subscriber_queue: 8,
            },
        )
    }

    fn phase_of(control: &ControlService) -> (fm_protocol::TitlePhase, u64, u64) {
        let states = control.title_states();
        (
            states[0].phase,
            states[0].phase_frame,
            states[0].phase_frames,
        )
    }

    #[test]
    fn animated_titles_report_their_phase_and_settle_within_their_command_ticks() {
        use fm_protocol::TitlePhase;

        let mut control = animated_title_service();
        let admin = principal(Role::Admin);
        let server = server_identity();
        let channel = OverlayChannelId::new(2).unwrap();

        let staged = prepared(
            control
                .prepare_submit(&admin, command("take", "take-key", take(0, Vec::new())), 0)
                .unwrap(),
        );
        // A take realizes its whole In phase: an idle boundary only after it.
        assert_eq!(staged.title_take_ticks(input(TITLE), channel), 20);
        assert!(staged.project(19).is_err());
        assert!(staged.project(20).is_ok());
        assert!(staged.commit().unwrap().is_accepted());
        assert_eq!(phase_of(&control), (TitlePhase::Idle, 0, 0));
        for _ in 0..5 {
            control.tick(&server).unwrap();
        }
        assert_eq!(phase_of(&control), (TitlePhase::In, 5, 20));
        for _ in 5..20 {
            control.tick(&server).unwrap();
        }
        assert_eq!(phase_of(&control), (TitlePhase::Hold, 0, 0));

        let staged = prepared(
            control
                .prepare_submit(&admin, command("off", "off-key", off_air()), 0)
                .unwrap(),
        );
        assert_eq!(staged.title_off_air_ticks(input(TITLE)), 10);
        assert!(staged.project(9).is_err());
        assert!(staged.project(10).is_ok());
        assert!(staged.commit().unwrap().is_accepted());
        for _ in 0..3 {
            control.tick(&server).unwrap();
        }
        assert_eq!(phase_of(&control), (TitlePhase::Out, 3, 10));
        for _ in 3..10 {
            control.tick(&server).unwrap();
        }
        assert_eq!(phase_of(&control), (TitlePhase::Idle, 0, 0));
        assert_eq!(control.titles_on_air(), Vec::new());
    }

    /// The off-air revision's realization, if `outcome` published one.
    fn realized(outcome: &crate::TickOutcome, revision: u64) -> Option<RuntimeLifecycleEvent> {
        outcome
            .runtime_events
            .iter()
            .find(|event| event.revision == revision)
            .map(|event| event.event.clone())
    }

    #[test]
    fn a_title_off_air_is_realized_when_its_out_phase_ends() {
        use fm_protocol::TitlePhase;

        let mut control = animated_title_service();
        let admin = principal(Role::Admin);
        let server = server_identity();
        let channel = OverlayChannelId::new(2).unwrap();
        let submit = |control: &mut ControlService, id: &str, payload| {
            let submission = control
                .submit(&admin, command(id, &format!("{id}-key"), payload), 0)
                .unwrap();
            assert!(submission.is_accepted(), "{submission:?}");
            control.engine.revision().get()
        };

        submit(&mut control, "take", take(0, Vec::new()));
        for _ in 0..20 {
            control.tick(&server).unwrap();
        }
        assert_eq!(phase_of(&control), (TitlePhase::Hold, 0, 0));

        // The 10-frame Out phase: nothing on its first nine frames, the
        // realization on its last, once the overlay is off.
        let off = submit(&mut control, "off", off_air());
        for frame in 0..9 {
            let outcome = control.tick(&server).unwrap();
            assert_eq!(realized(&outcome, off), None, "Out frame {frame}");
            assert!(control.work_in_flight());
            assert!(control.idle_engine_snapshot().is_err());
        }
        let outcome = control.tick(&server).unwrap();
        assert!(
            matches!(
                realized(&outcome, off),
                Some(RuntimeLifecycleEvent::Realized { ref domain, .. }) if domain == "switcher"
            ),
            "{:?}",
            outcome.runtime_events
        );
        assert_eq!(control.titles_on_air(), Vec::new());
        assert!(
            !control
                .engine
                .realized_switcher()
                .overlay(channel)
                .is_active()
        );
        assert!(!control.work_in_flight());

        // A re-take during Out supersedes the off-air, which is never
        // realized.
        submit(&mut control, "retake", take(0, Vec::new()));
        for _ in 0..20 {
            control.tick(&server).unwrap();
        }
        let off = submit(&mut control, "off-again", off_air());
        for _ in 0..3 {
            assert_eq!(realized(&control.tick(&server).unwrap(), off), None);
        }
        let retake = submit(&mut control, "retake-mid-out", take(0, Vec::new()));
        let outcome = control.tick(&server).unwrap();
        assert_eq!(
            realized(&outcome, off),
            Some(RuntimeLifecycleEvent::Superseded {
                by_revision: retake
            })
        );
        for _ in 0..30 {
            assert_eq!(realized(&control.tick(&server).unwrap(), off), None);
        }
        assert_eq!(phase_of(&control), (TitlePhase::Hold, 0, 0));
    }

    fn score(value: i64) -> Vec<TitleFieldUpdate> {
        vec![TitleFieldUpdate {
            field: field(SCORE),
            value: TitleFieldValue::Number(value),
        }]
    }

    fn set_fields(base_revision: u64, value: i64) -> CommandPayload {
        CommandPayload::SetTitleFields {
            input: WireInputId::from_domain(input(TITLE)),
            base_revision,
            require: TitleAirRequirement::OffAir,
            fields: score(value),
        }
    }

    fn take(base_revision: u64, fields: Vec<TitleFieldUpdate>) -> CommandPayload {
        CommandPayload::TakeTitle {
            input: WireInputId::from_domain(input(TITLE)),
            channel: WireOverlayChannelId::new(2).unwrap(),
            base_revision,
            fields,
        }
    }

    fn off_air() -> CommandPayload {
        CommandPayload::TitleOffAir {
            input: WireInputId::from_domain(input(TITLE)),
        }
    }

    fn denied(submission: &CommandSubmission) -> bool {
        matches!(
            &submission.output.result,
            CommandResult::Rejected { code, .. } if code == "permission_denied"
        )
    }

    fn expected_fields(name: &str, score: i64) -> Vec<TitleFieldUpdate> {
        vec![
            TitleFieldUpdate {
                field: field(NAME),
                value: TitleFieldValue::Text(name.into()),
            },
            TitleFieldUpdate {
                field: field(SCORE),
                value: TitleFieldValue::Number(score),
            },
        ]
    }

    #[test]
    fn authorization_matrix_covers_every_title_command_and_role() {
        let cases = [
            (Role::Viewer, set_fields(0, 1), false),
            (Role::Viewer, take(0, Vec::new()), false),
            (Role::Viewer, take(0, score(1)), false),
            (Role::Viewer, off_air(), false),
            (Role::Graphics, set_fields(0, 1), true),
            (Role::Graphics, take(0, Vec::new()), false),
            (Role::Graphics, take(0, score(1)), false),
            (Role::Graphics, off_air(), false),
            (Role::Operator, set_fields(0, 1), false),
            (Role::Operator, take(0, Vec::new()), true),
            (Role::Operator, take(0, score(1)), false),
            (Role::Operator, off_air(), true),
            (Role::Audio, take(0, Vec::new()), false),
            (Role::Admin, set_fields(0, 1), true),
            (Role::Admin, take(0, score(1)), true),
            (Role::Admin, off_air(), true),
        ];
        for (index, (role, payload, allowed)) in cases.into_iter().enumerate() {
            let mut control = title_service();
            let before = control.engine.snapshot().unwrap();
            let submission = control
                .submit(
                    &principal(role),
                    command(&format!("c{index}"), &format!("k{index}"), payload.clone()),
                    0,
                )
                .unwrap();
            assert_eq!(
                !denied(&submission),
                allowed,
                "{role:?} {payload:?}: {:?}",
                submission.output.result
            );
            if !allowed {
                assert_eq!(control.engine.snapshot().unwrap(), before);
            }
        }
    }

    #[test]
    fn take_with_fields_denial_is_cached_by_idempotency_key() {
        let mut control = title_service();
        let denied_submission = control
            .submit(
                &principal(Role::Operator),
                command("take", "take-key", take(0, score(5))),
                0,
            )
            .unwrap();
        assert!(denied(&denied_submission));
        let replayed = control
            .submit(
                &principal(Role::Admin),
                command("take-again", "take-key", take(0, score(5))),
                0,
            )
            .unwrap();
        assert!(replayed.replayed);
        assert_eq!(replayed.output.result, denied_submission.output.result);
        assert_eq!(
            control
                .engine
                .show()
                .title(input(TITLE))
                .unwrap()
                .fields
                .revision(),
            0
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn title_commands_publish_exact_events_snapshot_and_title_states() {
        let mut control = title_service();
        assert_eq!(
            control.snapshot().snapshot.titles,
            [TitleSummary {
                input: WireInputId::from_domain(input(TITLE)),
                field_revision: 0,
            }]
        );

        let saved = control
            .submit(
                &principal(Role::Graphics),
                command("save", "save-key", set_fields(0, 7)),
                0,
            )
            .unwrap();
        assert!(saved.is_accepted(), "{:?}", saved.output.result);
        assert_eq!(saved.output.events.len(), 1);
        assert_eq!(
            saved.output.events[0].payload,
            EventPayload::TitleChanged {
                input: WireInputId::from_domain(input(TITLE)),
                field_revision: 1,
                fields: expected_fields("Ana", 7),
                overlays: OverlayStatus::empty_channels(),
            }
        );

        let stale = control
            .submit(
                &principal(Role::Graphics),
                command("stale", "stale-key", set_fields(0, 8)),
                0,
            )
            .unwrap();
        assert!(
            matches!(&stale.output.result, CommandResult::Rejected { code, .. } if code != "permission_denied"),
            "{:?}",
            stale.output.result
        );

        let taken = control
            .submit(
                &principal(Role::Admin),
                command("take", "take-key", take(1, score(9))),
                0,
            )
            .unwrap();
        assert!(taken.is_accepted(), "{:?}", taken.output.result);
        assert_eq!(taken.output.events.len(), 1);
        let EventPayload::TitleChanged {
            input: changed,
            field_revision,
            fields,
            overlays,
        } = &taken.output.events[0].payload
        else {
            panic!("take publishes title_changed: {:?}", taken.output.events[0]);
        };
        assert_eq!(*changed, WireInputId::from_domain(input(TITLE)));
        assert_eq!(*field_revision, 2);
        assert_eq!(fields, &expected_fields("Ana", 9));
        assert_eq!(
            overlays[1].source,
            Some(WireInputId::from_domain(input(TITLE)))
        );
        assert!(overlays[1].active);

        let revision = control.diagnostics().current_revision;
        assert_eq!(
            control.title_states(),
            [TitleStateMessage {
                revision,
                input: WireInputId::from_domain(input(TITLE)),
                name: "Lower third".into(),
                field_revision: 2,
                phase: fm_protocol::TitlePhase::Idle,
                phase_frame: 0,
                phase_frames: 0,
                timeline: None,
                fields: vec![
                    TitleFieldStatus {
                        field: field(NAME),
                        name: "Name".into(),
                        value: TitleFieldValue::Text("Ana".into()),
                        exposed: true,
                    },
                    TitleFieldStatus {
                        field: field(SCORE),
                        name: "Score".into(),
                        value: TitleFieldValue::Number(9),
                        exposed: true,
                    },
                ],
            }]
        );
        assert_eq!(
            control.snapshot().snapshot.titles,
            [TitleSummary {
                input: WireInputId::from_domain(input(TITLE)),
                field_revision: 2,
            }]
        );

        let off = control
            .submit(
                &principal(Role::Operator),
                command("off", "off-key", off_air()),
                0,
            )
            .unwrap();
        assert!(off.is_accepted(), "{:?}", off.output.result);
        let EventPayload::DesiredSwitcher { overlays, .. } = &off.output.events[0].payload else {
            panic!(
                "off-air publishes desired_switcher: {:?}",
                off.output.events[0]
            );
        };
        assert!(!overlays[1].active);
    }
}

mod playlists {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistRunState, PlaylistStart,
        PlaylistTransitionKind,
    };

    use super::*;

    fn wire(value: u128) -> WireInputId {
        WireInputId::from_domain(input(value))
    }

    fn set_playlist() -> CommandPayload {
        CommandPayload::SetPlaylist {
            loop_mode: PlaylistLoopMode::Off,
            entries: vec![
                PlaylistEntrySpec {
                    id: 1,
                    input: wire(2),
                    duration_frames: 10,
                    transition: PlaylistTransitionKind::Cut,
                    transition_frames: 0,
                    enabled: true,
                },
                PlaylistEntrySpec {
                    id: 2,
                    input: wire(3),
                    duration_frames: 10,
                    transition: PlaylistTransitionKind::Fade,
                    transition_frames: 4,
                    enabled: true,
                },
            ],
        }
    }

    fn start_after(frames: u64) -> CommandPayload {
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Start {
                at: PlaylistStart::AfterFrames(frames),
                from: None,
            },
        }
    }

    fn denied(submission: &CommandSubmission) -> bool {
        matches!(
            &submission.output.result,
            CommandResult::Rejected { code, .. } if code == "permission_denied"
        )
    }

    fn accepted_frame(submission: &CommandSubmission) -> u64 {
        let CommandResult::Accepted {
            scheduled_frame: Some(frame),
            ..
        } = submission.output.result
        else {
            panic!("expected acceptance: {:?}", submission.output.result);
        };
        frame
    }

    fn submit_with_ticks(
        control: &mut ControlService,
        id: &str,
        payload: CommandPayload,
    ) -> (CommandSubmission, u64) {
        let prepared = prepared(
            control
                .prepare_submit(&principal(Role::Admin), command(id, id, payload), 0)
                .unwrap(),
        );
        let ticks = prepared.playlist_ticks();
        (prepared.commit().unwrap(), ticks)
    }

    #[test]
    fn authorization_matrix_covers_every_playlist_command() {
        let cases = [
            (Role::Viewer, set_playlist(), false),
            (Role::Viewer, start_after(0), false),
            (Role::Graphics, set_playlist(), true),
            (Role::Graphics, start_after(0), false),
            (Role::Operator, set_playlist(), false),
            (Role::Operator, start_after(0), true),
            (
                Role::Operator,
                CommandPayload::PlaylistAdvance { frame: 0 },
                true,
            ),
            (Role::Audio, start_after(0), false),
            (Role::Admin, set_playlist(), true),
            (Role::Admin, start_after(0), true),
        ];
        for (index, (role, payload, allowed)) in cases.into_iter().enumerate() {
            let mut control = service(8, 8);
            let submission = control
                .submit(
                    &principal(role),
                    command(&format!("c{index}"), &format!("k{index}"), payload.clone()),
                    0,
                )
                .unwrap();
            assert_eq!(
                !denied(&submission),
                allowed,
                "{role:?} {payload:?}: {:?}",
                submission.output.result
            );
        }
    }

    #[test]
    fn scheduled_go_cuts_and_fades_program_on_the_exact_frames() {
        let mut control = service(16, 16);
        let (set, ticks) = submit_with_ticks(&mut control, "set", set_playlist());
        assert_eq!(ticks, 1);
        let EventPayload::PlaylistChanged {
            frame,
            loop_mode,
            entries,
            state,
            program,
            preview,
        } = &set.output.events[0].payload
        else {
            panic!("expected playlist_changed: {:?}", set.output.events);
        };
        assert_eq!(
            (*frame, *loop_mode, entries.len(), *state),
            (0, PlaylistLoopMode::Off, 2, PlaylistRunState::Stopped)
        );
        assert_eq!((*program, *preview), (wire(1), wire(2)));
        control.tick(&server_identity()).unwrap();

        let (start, ticks) = submit_with_ticks(&mut control, "start", start_after(4));
        assert_eq!((accepted_frame(&start), ticks), (1, 1));
        assert!(matches!(
            start.output.events[0].payload,
            EventPayload::PlaylistChanged {
                state: PlaylistRunState::Armed {
                    start_frame: 5,
                    from: None
                },
                ..
            }
        ));
        control.tick(&server_identity()).unwrap();
        assert_eq!(control.engine.playlist_advance_frame(), Some(5));

        let wrong = control
            .submit(
                &principal(Role::Admin),
                command(
                    "early",
                    "early",
                    CommandPayload::PlaylistAdvance { frame: 4 },
                ),
                0,
            )
            .unwrap();
        assert!(!wrong.is_accepted());

        // Accepted ahead at frame 2, realized on exactly frame 5.
        let (go, ticks) = submit_with_ticks(
            &mut control,
            "go-1",
            CommandPayload::PlaylistAdvance { frame: 5 },
        );
        assert_eq!((accepted_frame(&go), ticks), (5, 4));
        let EventPayload::PlaylistChanged {
            frame,
            state,
            program,
            ..
        } = &go.output.events[0].payload
        else {
            panic!("expected playlist_changed");
        };
        assert_eq!((*frame, *program), (5, wire(2)));
        assert!(matches!(
            state,
            PlaylistRunState::Playing {
                entry: 1,
                started_frame: 5,
                ..
            }
        ));
        for expected_frame in 2..5 {
            let tick = control.tick(&server_identity()).unwrap();
            assert_eq!(tick.frame.frame.get(), expected_frame);
            assert_eq!(tick.frame.program.primary, input(1));
            assert_eq!(tick.runtime_events, Vec::new());
        }
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(tick.frame.frame.get(), 5);
        assert_eq!(tick.frame.program.primary, input(2));
        assert_eq!(tick.runtime_events.len(), 1);
        assert!(control.idle_engine_snapshot().is_ok());

        // The second entry fades in from frame 15 for four frames.
        for _ in 6..15 {
            control.tick(&server_identity()).unwrap();
        }
        assert_eq!(control.engine.playlist_advance_frame(), Some(15));
        let (go, ticks) = submit_with_ticks(
            &mut control,
            "go-2",
            CommandPayload::PlaylistAdvance { frame: 15 },
        );
        assert_eq!((accepted_frame(&go), ticks), (15, 4));
        let mut realized = Vec::new();
        for _ in 0..ticks {
            let tick = control.tick(&server_identity()).unwrap();
            realized.extend(tick.runtime_events);
        }
        assert_eq!(realized.len(), 1);
        assert_eq!(control.engine.realized_switcher().program(), input(3));
        assert!(control.idle_engine_snapshot().is_ok());
    }

    #[test]
    fn snapshots_announce_the_playlist_that_follows_them() {
        let mut control = service(16, 16);
        // The empty playlist is implied: nothing follows the snapshot.
        assert!(!control.snapshot().snapshot.playlist_follows);
        assert_eq!(control.playlist_state(), None);
        assert_eq!(
            control.snapshot().snapshot.frame_rate,
            fm_types::FrameRate::new(60, 1).unwrap()
        );

        submit_with_ticks(&mut control, "set", set_playlist());
        control.tick(&server_identity()).unwrap();
        submit_with_ticks(&mut control, "start", start_after(4));
        control.tick(&server_identity()).unwrap();
        let snapshot = &control.snapshot().snapshot;
        assert!(snapshot.playlist_follows);
        let state = control.playlist_state().expect("an armed playlist follows");
        assert_eq!(state.revision, snapshot.revision);
        assert_eq!(state.frame, control.frame_cursor());
        assert_eq!(state.loop_mode, PlaylistLoopMode::Off);
        assert_eq!(
            state.entries,
            match set_playlist() {
                CommandPayload::SetPlaylist { entries, .. } => entries,
                _ => unreachable!(),
            }
        );
        assert_eq!(
            state.state,
            PlaylistRunState::Armed {
                start_frame: 5,
                from: None
            }
        );

        // Clearing a stopped playlist returns to the implied empty one.
        submit_with_ticks(
            &mut control,
            "stop",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Stop,
            },
        );
        control.tick(&server_identity()).unwrap();
        submit_with_ticks(
            &mut control,
            "clear",
            CommandPayload::SetPlaylist {
                loop_mode: PlaylistLoopMode::Off,
                entries: Vec::new(),
            },
        );
        assert!(!control.snapshot().snapshot.playlist_follows);
        assert_eq!(control.playlist_state(), None);
        // A looping empty playlist still differs from the implied one.
        submit_with_ticks(
            &mut control,
            "loop",
            CommandPayload::SetPlaylist {
                loop_mode: PlaylistLoopMode::All,
                entries: Vec::new(),
            },
        );
        assert!(control.snapshot().snapshot.playlist_follows);
        assert_eq!(
            control.playlist_state().map(|state| state.loop_mode),
            Some(PlaylistLoopMode::All)
        );
    }
}

/// RC-017: cue list edits are graphics project edits that publish the whole
/// list and undo; GO and standby are operator transitions that publish the
/// run; the clock's `cue_advance` is accepted only for the due frame; and
/// snapshots announce the `cue_list_state` records that follow them.
mod cues {
    use fm_protocol::{
        CueListSpec, CueOperation, CuePendingStatus, CueRunStatus, TransitionFavoriteEffect,
        WireCue, WireCueAction,
    };

    use super::*;

    fn wire(value: u128) -> WireInputId {
        WireInputId::from_domain(input(value))
    }

    fn cut(value: u128) -> WireCueAction {
        WireCueAction::Take {
            input: Some(fm_types::InputTarget::Fixed(wire(value))),
            effect: TransitionFavoriteEffect::Cut,
            duration_frames: 0,
        }
    }

    fn spec() -> CueListSpec {
        CueListSpec {
            name: "Show".into(),
            cues: vec![
                WireCue {
                    id: 10,
                    name: "Open".into(),
                    auto_follow: false,
                    actions: vec![cut(2), WireCueAction::Wait { frames: 3 }, cut(3)],
                },
                WireCue {
                    id: 20,
                    name: "Close".into(),
                    auto_follow: false,
                    actions: vec![cut(1)],
                },
            ],
        }
    }

    fn set() -> CommandPayload {
        CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(spec()),
            position: None,
        }
    }

    fn go() -> CommandPayload {
        CommandPayload::CueControl {
            list: 7,
            operation: CueOperation::Go {
                cue: None,
                targets: fm_protocol::WireTargetBindings::default(),
            },
        }
    }

    fn denied(submission: &CommandSubmission) -> bool {
        matches!(
            &submission.output.result,
            CommandResult::Rejected { code, .. } if code == "permission_denied"
        )
    }

    fn submit_with_ticks(
        control: &mut ControlService,
        id: &str,
        payload: CommandPayload,
    ) -> (CommandSubmission, u64) {
        let prepared = prepared(
            control
                .prepare_submit(&principal(Role::Admin), command(id, id, payload), 0)
                .unwrap(),
        );
        let ticks = prepared.cue_ticks();
        (prepared.commit().unwrap(), ticks)
    }

    fn run(submission: &CommandSubmission) -> &fm_protocol::CueRunChange {
        match &submission.output.events[0].payload {
            EventPayload::CueRunChanged(change) => change,
            other => panic!("expected cue_run_changed: {other:?}"),
        }
    }

    #[test]
    fn authorization_matrix_covers_every_cue_command() {
        let cases = [
            (Role::Viewer, set(), false),
            (Role::Viewer, go(), false),
            (Role::Graphics, set(), true),
            (Role::Graphics, go(), false),
            (Role::Operator, set(), false),
            (Role::Operator, go(), true),
            (Role::Audio, go(), false),
            (Role::Admin, set(), true),
        ];
        for (index, (role, payload, allowed)) in cases.into_iter().enumerate() {
            let mut control = service(8, 8);
            if matches!(payload, CommandPayload::CueControl { .. }) {
                submit_with_ticks(&mut control, "seed", set());
                control.tick(&server_identity()).unwrap();
            }
            let submission = control
                .submit(
                    &principal(role),
                    command(&format!("c{index}"), &format!("k{index}"), payload.clone()),
                    0,
                )
                .unwrap();
            assert_eq!(
                !denied(&submission),
                allowed,
                "{role:?} {payload:?}: {:?}",
                submission.output.result
            );
        }
    }

    #[test]
    fn go_fires_to_the_wait_and_the_advance_lands_on_its_frame() {
        let mut control = service(16, 16);
        assert_eq!(control.snapshot().snapshot.cue_list_states, 0);
        let (edit, ticks) = submit_with_ticks(&mut control, "set", set());
        assert_eq!(ticks, 1);
        let EventPayload::CueListChanged(list) = &edit.output.events[0].payload else {
            panic!("expected cue_list_changed: {:?}", edit.output.events);
        };
        assert_eq!(
            (list.id, &list.spec, list.state.standby),
            (7, &spec(), Some(10))
        );
        control.tick(&server_identity()).unwrap();

        let (fired, ticks) = submit_with_ticks(&mut control, "go", go());
        let change = run(&fired).clone();
        assert_eq!((change.list, change.fired.as_slice()), (7, [10].as_slice()));
        assert_eq!((change.program, change.preview), (wire(2), wire(1)));
        let due = change.frame + 3;
        assert_eq!(
            change.state,
            CueRunStatus {
                standby: Some(20),
                live: Some(10),
                pending: Some(CuePendingStatus {
                    cue: 10,
                    action: 2,
                    due_frame: due,
                }),
                played: vec![10],
                fault: None,
                armed: None,
                targets: None,
            }
        );
        assert_eq!(ticks, 1);
        control.tick(&server_identity()).unwrap();
        assert_eq!(control.engine.realized_switcher().program(), input(2));
        assert_eq!(control.cue_advance_frame(), Some((7, due)));

        // Only the due advance is taken.
        let early = control
            .submit(
                &principal(Role::Admin),
                command(
                    "early",
                    "early",
                    CommandPayload::CueAdvance {
                        list: 7,
                        frame: due - 1,
                    },
                ),
                0,
            )
            .unwrap();
        assert!(!early.is_accepted());

        // Accepted ahead, realized on exactly the due frame.
        let (advance, ticks) = submit_with_ticks(
            &mut control,
            "advance",
            CommandPayload::CueAdvance {
                list: 7,
                frame: due,
            },
        );
        let change = run(&advance).clone();
        assert_eq!((change.frame, change.program), (due, wire(3)));
        // Until the accepted advance lands, other commands are held off.
        let held = control
            .submit(
                &principal(Role::Admin),
                command("cut", "cut", CommandPayload::Cut),
                0,
            )
            .unwrap();
        assert!(
            matches!(
                &held.output.result,
                CommandResult::Rejected { reason: Some(fm_protocol::RefusalReason::CueScheduled { frame }), .. }
                    if *frame == due
            ),
            "{:?}",
            held.output.result
        );
        assert_eq!(change.fired, Vec::<u64>::new());
        assert_eq!(change.state.pending, None);
        let start = control.frame_cursor();
        assert_eq!(ticks, due - start + 1);
        for _ in start..due {
            let tick = control.tick(&server_identity()).unwrap();
            assert_eq!(tick.frame.program.primary, input(2));
        }
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(tick.frame.frame.get(), due);
        assert_eq!(tick.frame.program.primary, input(3));
        assert_eq!(control.cue_advance_frame(), None);

        // The snapshot announces the list that follows it.
        let snapshot = control.snapshot().snapshot.clone();
        assert_eq!(snapshot.cue_list_states, 1);
        let states = control.cue_list_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].revision, snapshot.revision);
        assert_eq!(
            (states[0].list.id, &states[0].list.state),
            (7, &change.state)
        );
    }

    #[test]
    fn list_edits_undo_and_runtime_go_does_not() {
        let mut control = service(16, 16);
        submit_with_ticks(&mut control, "set", set());
        control.tick(&server_identity()).unwrap();
        submit_with_ticks(
            &mut control,
            "standby",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Standby { cue: 20 },
            },
        );
        control.tick(&server_identity()).unwrap();
        // Undo skips the standby (not undoable) and reverts the list's
        // creation.
        let (undo, _) = submit_with_ticks(
            &mut control,
            "undo",
            CommandPayload::Undo {
                expected_edit: None,
            },
        );
        assert!(
            matches!(
                undo.output.events[0].payload,
                EventPayload::CueListRemoved { list: 7 }
            ),
            "{:?}",
            undo.output.events
        );
        control.tick(&server_identity()).unwrap();
        assert_eq!(control.snapshot().snapshot.cue_list_states, 0);
        let (redo, _) = submit_with_ticks(
            &mut control,
            "redo",
            CommandPayload::Redo {
                expected_edit: None,
            },
        );
        assert!(matches!(
            &redo.output.events[0].payload,
            EventPayload::CueListChanged(list) if list.spec == spec()
        ));
        let (remove, _) = submit_with_ticks(
            &mut control,
            "remove",
            CommandPayload::RemoveCueList { list: 7 },
        );
        assert!(matches!(
            remove.output.events[0].payload,
            EventPayload::CueListRemoved { list: 7 }
        ));
    }
}

/// SW-019 stage 3: grade commands are `EditProject`, publish
/// `grade_changed`, follow snapshots as `grade_state` records, and undo
/// through the shared history.
#[test]
fn grade_commands_are_project_edits_with_state_records_and_undo() {
    use fm_protocol::{GradeChange, GradeEntry, WireGradeSource};

    let mut control = service(16, 16);
    let warm = Box::new(fm_grade::builtin_preset("warm").unwrap().grade.clone());
    let grade_id = core::num::NonZeroU32::new(4).unwrap();
    let add = CommandPayload::AddLibraryGrade {
        grade: grade_id,
        name: "Warm".into(),
        value: warm.clone(),
        index: None,
    };
    let set = CommandPayload::SetInputGrade {
        input: WireInputId::from_domain(input(2)),
        source: Some(WireGradeSource::Library(grade_id)),
        gesture: None,
    };
    for payload in [&add, &set] {
        assert_eq!(
            control.required_classes(payload).primary(),
            CommandClass::EditProject
        );
    }

    let denied = control
        .submit(&principal(Role::Viewer), command("v", "v", add.clone()), 0)
        .unwrap();
    assert!(!denied.is_accepted());

    let added = control
        .submit(&principal(Role::Graphics), command("a", "a", add), 0)
        .unwrap();
    assert!(added.is_accepted(), "{:?}", added.output.result);
    assert_eq!(
        added.output.events[0].payload,
        EventPayload::GradeChanged(GradeChange::Library {
            grade: grade_id,
            index: 0,
            name: "Warm".into(),
            value: warm.clone(),
        })
    );
    let set = control
        .submit(&principal(Role::Graphics), command("s", "s", set), 0)
        .unwrap();
    assert!(set.is_accepted(), "{:?}", set.output.result);

    assert_eq!(control.snapshot().snapshot.grade_states, 2);
    let states = control.grade_states();
    assert_eq!(
        states.iter().map(|state| &state.entry).collect::<Vec<_>>(),
        [
            &GradeEntry::Library {
                grade: grade_id,
                name: "Warm".into(),
                value: warm,
            },
            &GradeEntry::Input {
                input: WireInputId::from_domain(input(2)),
                source: WireGradeSource::Library(grade_id),
            },
        ]
    );
    let revision = control.snapshot().snapshot.revision;
    assert!(states.iter().all(|state| state.revision == revision));
    assert!(control.compiled_grades().unwrap().input(input(2)).is_some());

    // Undo needs the undone edit's class and clears the input grade.
    let undo = CommandPayload::Undo {
        expected_edit: None,
    };
    assert_eq!(
        control.required_classes(&undo).additional(),
        [CommandClass::EditProject]
    );
    let undone = control
        .submit(&principal(Role::Graphics), command("u", "u", undo), 0)
        .unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        undone.output.events[0].payload,
        EventPayload::GradeChanged(GradeChange::Input {
            input: WireInputId::from_domain(input(2)),
            source: None,
        })
    );
    assert_eq!(control.snapshot().snapshot.grade_states, 1);
}

/// Regression: an animation edit whose resulting clip has no wire encoding
/// (it exceeds `MAX_TITLE_ANIMATION_BYTES`) is refused. A small stretch
/// command can grow a clip that fits past the bound; accepting it made the
/// `title_animation_changed` event and every later snapshot's
/// `title_animation_state` unencodable, locking every session out.
#[test]
#[allow(clippy::too_many_lines)]
fn title_animation_edits_refuse_clips_without_a_wire_encoding() {
    use fm_engine::EngineTitle;
    use fm_protocol::{MAX_TITLE_ANIMATION_BYTES, WireMessage, encode_line, title_clip_wire_bytes};
    use fm_titles::fm_animation::{
        Clip, CubicBezier, Easing, Keyframe, MAX_PHASE_FRAMES, Phase, Property, Timebase, Track,
        TrackKey, Value,
    };
    use fm_titles::{
        Bounds, Color, Element, ElementId, ElementKind, Style, TemplateId, TitleAnimation,
        TitleFieldSet, TitleTemplate,
    };

    const KEYS: u32 = 128;
    let bezier = Easing::CubicBezier(CubicBezier::new(65_536, -65_536, 65_536, 131_072).unwrap());
    let clip = |tracks: u128| {
        Clip::from_tracks(
            Timebase::new(FrameRate::new(60, 1).unwrap()),
            KEYS - 1,
            0,
            (1..=tracks)
                .map(|element| {
                    Track::new(
                        TrackKey::new(
                            ElementId::new(NonZeroU128::new(element).unwrap()),
                            Phase::In,
                            Property::PositionX,
                        ),
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
    };
    // The most tracks whose clip still fits the wire bound.
    let tracks = (1..=32_u128)
        .take_while(|tracks| {
            title_clip_wire_bytes(&title_clip_to_wire(&clip(*tracks))) <= MAX_TITLE_ANIMATION_BYTES
        })
        .last()
        .unwrap();
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower".into(),
        width: 32,
        height: 18,
        background: Color::TRANSPARENT,
        fields: Vec::new(),
        elements: (1..=tracks)
            .map(|element| Element {
                id: ElementId::new(NonZeroU128::new(element).unwrap()),
                name: format!("box {element}"),
                bounds: Bounds {
                    x: 2,
                    y: 2,
                    width: 8,
                    height: 8,
                },
                z_index: 0,
                visible: true,
                style: Style::default(),
                color_field: None,
                kind: ElementKind::Rectangle,
            })
            .collect(),
        animation: TitleAnimation::new(clip(tracks), 0),
    };
    let mut stretched = template.clone();
    stretched
        .stretch_animation(Phase::In, MAX_PHASE_FRAMES)
        .unwrap();
    assert!(
        title_clip_wire_bytes(&title_clip_to_wire(&stretched.animation.clip))
            > MAX_TITLE_ANIMATION_BYTES,
        "the stretched clip must outgrow the wire bound"
    );
    let mut show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap();
    show.set_title(
        input(3),
        EngineTitle::new("Lower", TitleFieldSet::from_template(&template).unwrap())
            .with_template("titles/lower.fmtitle", template.clone()),
    )
    .unwrap();
    let mut control = ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 8,
            max_subscribers: 4,
            subscriber_queue: 8,
        },
    );
    let refused = control
        .submit(
            &principal(Role::Graphics),
            command(
                "stretch",
                "stretch",
                CommandPayload::StretchTitleAnimation {
                    input: WireInputId::from_domain(input(3)),
                    base_revision: 0,
                    phase: Phase::In,
                    frames: MAX_PHASE_FRAMES,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&refused), Some("invalid_command"));
    assert_eq!(rejection_field_code(&refused), Some("too_large"));
    let states = control.title_animation_states();
    assert_eq!(states[0].animation_revision, 0);
    for state in states {
        encode_line(&WireMessage::TitleAnimationState(state)).unwrap();
    }
}

/// GX-006 (Protocol 2.90): a field's browser exposure is a template-wide
/// project edit Graphics may make; it publishes `title_exposure_changed`
/// for every title sharing the template, shows in `title_state`, and is not
/// undoable.
#[test]
#[allow(clippy::too_many_lines)]
fn title_field_exposure_is_a_template_wide_project_edit() {
    use fm_engine::EngineTitle;
    use fm_protocol::{TitleFieldExposure, WireTitleFieldId};
    use fm_titles::{
        Color, FieldDefinition, FieldId, FieldValue, TemplateId, TitleAnimation, TitleFieldSet,
        TitleTemplate,
    };

    let definition = |id: u128, name: &str| FieldDefinition {
        id: FieldId::new(NonZeroU128::new(id).unwrap()),
        name: name.into(),
        default: FieldValue::Text(name.into()),
        exposed: true,
    };
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower".into(),
        width: 32,
        height: 18,
        background: Color::TRANSPARENT,
        fields: vec![definition(1, "Name"), definition(2, "Secret")],
        elements: Vec::new(),
        animation: TitleAnimation::default(),
    };
    let mut show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3), input(4)]),
        input(1),
        input(2),
    )
    .unwrap();
    for title in [input(3), input(4)] {
        show.set_title(
            title,
            EngineTitle::new("Lower", TitleFieldSet::from_template(&template).unwrap())
                .with_template("titles/lower.fmtitle", template.clone()),
        )
        .unwrap();
    }
    let mut control = ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 8,
            max_subscribers: 4,
            subscriber_queue: 8,
        },
    );
    let field = |id: u128| WireTitleFieldId::new(NonZeroU128::new(id).unwrap());
    let hide = CommandPayload::SetTitleFieldExposure {
        input: WireInputId::from_domain(input(3)),
        field: field(2),
        exposed: false,
    };
    assert_eq!(
        control.required_classes(&hide).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            command("o", "o", hide.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));

    let hidden = control
        .submit(&principal(Role::Graphics), command("h", "h", hide), 0)
        .unwrap();
    assert!(hidden.is_accepted(), "{:?}", hidden.output.result);
    assert_eq!(
        hidden.output.events[0].payload,
        EventPayload::TitleExposureChanged {
            inputs: vec![
                WireInputId::from_domain(input(3)),
                WireInputId::from_domain(input(4)),
            ],
            fields: vec![
                TitleFieldExposure {
                    field: field(1),
                    exposed: true,
                },
                TitleFieldExposure {
                    field: field(2),
                    exposed: false,
                },
            ],
        }
    );
    assert_wire_exact(&control, &hidden);
    // Both titles sharing the template report it; values do not change.
    for state in control.title_states() {
        let flags: Vec<bool> = state.fields.iter().map(|status| status.exposed).collect();
        assert_eq!(flags, [true, false], "{state:?}");
        assert_eq!(state.field_revision, 0);
    }
    let template_now = control.title_template(input(4)).unwrap();
    assert!(!template_now.template.fields[1].exposed);
    // Not undoable: nothing entered the history.
    assert_eq!(hidden.output.events[0].edit_history.undo_depth, 0);

    let unknown = control
        .submit(
            &principal(Role::Graphics),
            command(
                "u",
                "u",
                CommandPayload::SetTitleFieldExposure {
                    input: WireInputId::from_domain(input(3)),
                    field: field(7),
                    exposed: true,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));
    let not_title = control
        .submit(
            &principal(Role::Graphics),
            command(
                "n",
                "n",
                CommandPayload::SetTitleFieldExposure {
                    input: WireInputId::from_domain(input(1)),
                    field: field(1),
                    exposed: true,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&not_title), Some("not_found"));
}

/// GX-011 stage 4a: title animation edits are `EditProject`, revision
/// checked, publish `title_animation_changed`, follow snapshots as
/// `title_animation_state` records, and undo through the shared history
/// with a revision-checked replay command.
#[test]
#[allow(clippy::too_many_lines)]
fn title_animation_edits_are_checked_project_edits_with_state_records_and_undo() {
    use fm_engine::EngineTitle;
    use fm_protocol::{TitleAnimationStateMessage, WireTitleElementId};
    use fm_titles::fm_animation::{AnimationPreset, Phase};
    use fm_titles::{
        Bounds, Color, Element, ElementId, ElementKind, Style, TemplateId, TitleAnimation,
        TitleFieldSet, TitleTemplate,
    };

    let element = NonZeroU128::new(7).unwrap();
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower".into(),
        width: 32,
        height: 18,
        background: Color::TRANSPARENT,
        fields: Vec::new(),
        elements: vec![Element {
            id: ElementId::new(element),
            name: "box".into(),
            bounds: Bounds {
                x: 2,
                y: 2,
                width: 8,
                height: 8,
            },
            z_index: 0,
            visible: true,
            style: Style::default(),
            color_field: None,
            kind: ElementKind::Rectangle,
        }],
        animation: TitleAnimation::default(),
    };
    let mut show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap();
    show.set_title(
        input(3),
        EngineTitle::new("Lower", TitleFieldSet::from_template(&template).unwrap())
            .with_template("titles/lower.fmtitle", template.clone()),
    )
    .unwrap();
    let mut control = ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 8,
            max_subscribers: 4,
            subscriber_queue: 8,
        },
    );
    let title = WireInputId::from_domain(input(3));
    let initial = control.title_animation_states();
    assert_eq!(
        initial,
        vec![TitleAnimationStateMessage {
            revision: control.snapshot().snapshot.revision,
            input: title,
            animation_revision: 0,
            clip: Box::new(title_clip_to_wire(&template.animation.clip)),
        }]
    );

    let preset = CommandPayload::ApplyTitleAnimationPreset {
        input: title,
        base_revision: 0,
        element: WireTitleElementId::new(element),
        phase: Phase::In,
        preset: AnimationPreset::Fade,
        frames: 12,
    };
    assert_eq!(
        control.required_classes(&preset).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            command("o", "o", preset.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));

    let applied = control
        .submit(&principal(Role::Graphics), command("p", "p", preset), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    let mut expected = template.clone();
    expected
        .apply_animation_preset(
            ElementId::new(element),
            Phase::In,
            AnimationPreset::Fade,
            12,
        )
        .unwrap();
    let edited_clip = Box::new(title_clip_to_wire(&expected.animation.clip));
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::TitleAnimationChanged {
            inputs: vec![title],
            animation_revision: 1,
            clip: edited_clip.clone(),
        }
    );
    assert_wire_exact(&control, &applied);
    assert_eq!(control.title_animation_states()[0].clip, edited_clip);

    // A stale base revision is a conflict.
    let stale = control
        .submit(
            &principal(Role::Graphics),
            command(
                "s",
                "s",
                CommandPayload::StretchTitleAnimation {
                    input: title,
                    base_revision: 0,
                    phase: Phase::In,
                    frames: 24,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&stale), Some("conflict"));
    assert_eq!(rejection_field_code(&stale), Some("revision_conflict"));

    // Undo journals the restored clip as a revision-checked set.
    let message = undo("undo", None);
    let prepared = prepared(
        control
            .prepare_submit(&principal(Role::Graphics), message.clone(), 0)
            .unwrap(),
    );
    assert_eq!(
        prepared.replay_command(&message).payload,
        CommandPayload::SetTitleAnimation {
            input: title,
            base_revision: 1,
            clip: Box::new(title_clip_to_wire(&template.animation.clip)),
        }
    );
    let undone = prepared.commit().unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        undone.output.events[0].payload,
        EventPayload::TitleAnimationChanged {
            inputs: vec![title],
            animation_revision: 2,
            clip: Box::new(title_clip_to_wire(&template.animation.clip)),
        }
    );

    // The journaled undo only applies on the state it was journaled at.
    let replayed = control
        .submit(
            &principal(Role::Graphics),
            command(
                "r",
                "r",
                CommandPayload::SetTitleAnimation {
                    input: title,
                    base_revision: 1,
                    clip: edited_clip.clone(),
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&replayed), Some("conflict"));

    let redone = control
        .submit(&principal(Role::Graphics), redo("redo", None), 0)
        .unwrap();
    assert!(redone.is_accepted(), "{:?}", redone.output.result);
    let states = control.title_animation_states();
    assert_eq!(states[0].animation_revision, 3);
    assert_eq!(states[0].clip, edited_clip);
}

/// GX-011, Protocol 2.30: every snapshot's `title_layout_state` records
/// carry each title template's canvas and element outlines exactly as the
/// template holds them (names cleaned and bounded for the wire, kinds
/// mapped, hidden elements kept), one per title in snapshot order; a title
/// without a template reports the empty layout.
#[test]
#[allow(clippy::too_many_lines)]
fn title_layout_states_match_each_template() {
    use fm_engine::EngineTitle;
    use fm_protocol::{
        MAX_TITLE_ELEMENT_NAME_BYTES, TitleLayoutStateMessage, WireMessage, WireTitleElementId,
        WireTitleElementKind, WireTitleElementOutline, WireTitleLayout, encode_line,
    };
    use fm_titles::fm_animation::Rgba;
    use fm_titles::{
        Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue,
        ImageFilter, ImageFit, ImageSource, Style, TemplateId, TitleAnimation, TitleFieldSet,
        TitleTemplate,
    };

    struct Expected<'a> {
        id: u128,
        name: &'a str,
        kind: WireTitleElementKind,
        bounds: (i32, i32, u32, u32),
        opacity: u8,
        fill: Rgba,
        visible: bool,
    }

    let field = |value: u128| FieldId::new(NonZeroU128::new(value).unwrap());
    let element = |id: u128, name: &str, kind, bounds, style, visible| Element {
        id: ElementId::new(NonZeroU128::new(id).unwrap()),
        name: name.to_owned(),
        bounds,
        z_index: 0,
        visible,
        style,
        color_field: None,
        kind,
    };
    let long_name = format!("Sponsor\tlogo {}", "ñ".repeat(60));
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower third".into(),
        width: 1_920,
        height: 1_080,
        background: Color::TRANSPARENT,
        fields: vec![
            FieldDefinition {
                id: field(1),
                name: "Name".into(),
                default: FieldValue::Text("Ana".into()),
                exposed: true,
            },
            FieldDefinition {
                id: field(2),
                name: "Logo".into(),
                default: FieldValue::Image(ImageSource::new("asset://logo.png")),
                exposed: true,
            },
        ],
        elements: vec![
            element(
                30,
                "Bar",
                ElementKind::Rectangle,
                Bounds {
                    x: -40,
                    y: 840,
                    width: 1_000,
                    height: 140,
                },
                Style {
                    fill: Color::new(10, 20, 200, 230),
                    opacity: 120,
                    ..Style::default()
                },
                false,
            ),
            element(
                7,
                "Name",
                ElementKind::Text { field: field(1) },
                Bounds {
                    x: 160,
                    y: 860,
                    width: 700,
                    height: 60,
                },
                Style::default(),
                true,
            ),
            element(
                12,
                &long_name,
                ElementKind::Image {
                    field: field(2),
                    fit: ImageFit::Contain,
                    filter: ImageFilter::Bilinear,
                },
                Bounds {
                    x: 1_700,
                    y: 60,
                    width: 160,
                    height: 160,
                },
                Style::default(),
                true,
            ),
        ],
        animation: TitleAnimation::default(),
    };
    let mut show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3), input(4)]),
        input(1),
        input(2),
    )
    .unwrap();
    let fields = TitleFieldSet::from_template(&template).unwrap();
    show.set_title(
        input(3),
        EngineTitle::new("Lower third", fields.clone())
            .with_template("titles/lower.fmtitle", template),
    )
    .unwrap();
    show.set_title(input(4), EngineTitle::new("Bare", fields))
        .unwrap();
    let control = ControlService::new(
        Engine::new(
            show,
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 8,
            max_subscribers: 4,
            subscriber_queue: 8,
        },
    );

    // The name's tab becomes a space and it is cut at a character boundary.
    let mut wire_name = "Sponsor logo ".to_owned();
    while wire_name.len() + 'ñ'.len_utf8() <= MAX_TITLE_ELEMENT_NAME_BYTES {
        wire_name.push('ñ');
    }
    let white = Rgba::new(255, 255, 255, 255);
    let expected = [
        Expected {
            id: 30,
            name: "Bar",
            kind: WireTitleElementKind::Rectangle,
            bounds: (-40, 840, 1_000, 140),
            opacity: 120,
            fill: Rgba::new(10, 20, 200, 230),
            visible: false,
        },
        Expected {
            id: 7,
            name: "Name",
            kind: WireTitleElementKind::Text,
            bounds: (160, 860, 700, 60),
            opacity: 255,
            fill: white,
            visible: true,
        },
        Expected {
            id: 12,
            name: &wire_name,
            kind: WireTitleElementKind::Image,
            bounds: (1_700, 60, 160, 160),
            opacity: 255,
            fill: white,
            visible: true,
        },
    ]
    .into_iter()
    .map(|expected| WireTitleElementOutline {
        id: WireTitleElementId::new(NonZeroU128::new(expected.id).unwrap()),
        name: expected.name.to_owned(),
        kind: expected.kind,
        x: expected.bounds.0,
        y: expected.bounds.1,
        width: expected.bounds.2,
        height: expected.bounds.3,
        opacity: expected.opacity,
        fill: expected.fill,
        visible: expected.visible,
    })
    .collect();
    let revision = control.snapshot().snapshot.revision;
    let states = control.title_layout_states();
    assert_eq!(
        states,
        vec![
            TitleLayoutStateMessage {
                revision,
                input: WireInputId::from_domain(input(3)),
                layout: Box::new(WireTitleLayout {
                    width: 1_920,
                    height: 1_080,
                    elements: expected,
                }),
            },
            TitleLayoutStateMessage {
                revision,
                input: WireInputId::from_domain(input(4)),
                layout: Box::new(WireTitleLayout::default()),
            },
        ]
    );
    // One per title, in the snapshot's title order.
    let announced: Vec<_> = control
        .snapshot()
        .snapshot
        .titles
        .iter()
        .map(|title| title.input)
        .collect();
    let laid_out: Vec<_> = states.iter().map(|state| state.input).collect();
    assert_eq!(announced, laid_out);
    for state in states {
        encode_line(&WireMessage::TitleLayoutState(state)).unwrap();
    }
}

/// A scene with a named, turned layer and a plain one, shown by Scene
/// inputs 4 and 5 (GX-011 stage 5), and scene 41 that no Scene input shows,
/// one layer drawing scene 40.
fn scene_control() -> (ControlService, fm_model::Scene) {
    use fm_model::{
        Layer, LayerGeometry, LayerId, Rgba8, Rotation, Scene, SceneAnimation, SourceRef,
    };
    use fm_types::SceneId;

    let layer = |id: u32, name: &str, rotation| Layer {
        id: LayerId::new(id).unwrap(),
        name: name.into(),
        source: SourceRef::Input(input(1)),
        enabled: true,
        geometry: LayerGeometry::new(100, 50, 640, 360, rotation),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: 200,
        z_order: 0,
    };
    let scene = Scene {
        id: SceneId::new(NonZeroU128::new(40).unwrap()),
        name: "Two up".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(3, "Guest\tcam", Rotation::Deg90),
            layer(9, "Host", Rotation::Deg0),
        ],
        animation: SceneAnimation::default(),
    };
    let mut show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(4), input(5)]),
        input(1),
        input(2),
    )
    .unwrap();
    show.set_scene_input(input(4), scene.clone(), 1_920, 1_080)
        .unwrap();
    show.set_scene_input(input(5), scene.clone(), 1_920, 1_080)
        .unwrap();
    show.set_scene(
        Scene {
            id: SceneId::new(NonZeroU128::new(41).unwrap()),
            name: "Spare\nwall".into(),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![Layer {
                source: SourceRef::Scene(scene.id),
                ..layer(2, "Nested", Rotation::Deg0)
            }],
            animation: SceneAnimation::default(),
        },
        1_920,
        1_080,
    );
    let control = ControlService::new(
        Engine::new(
            show,
            FrameRate::new(60, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        ),
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 8,
            max_subscribers: 4,
            subscriber_queue: 8,
        },
    );
    (control, scene)
}

/// GX-011 stage 5, Protocol 2.31 (2.35): a snapshot lists its Scene inputs
/// and every scene and is followed by one `scene_animation_state` per Scene
/// input and one `scene_layout_state` per scene; scene animation edits are revision-checked `EditProject`
/// commands shared by every Scene input showing the scene, and undo
/// journals the restored clip as a revision-checked set.
#[test]
#[allow(clippy::too_many_lines)]
fn scene_animation_edits_are_checked_project_edits_with_state_records_and_undo() {
    use fm_model::fm_animation::{AnimationPreset, Phase};
    use fm_protocol::{
        SceneAnimationStateMessage, WireSceneId, WireSceneLayerId, WireSceneLayerOutline,
        WireSceneLayerSource, encode_line,
    };

    let (mut control, scene) = scene_control();
    let scenes = [
        WireInputId::from_domain(input(4)),
        WireInputId::from_domain(input(5)),
    ];
    assert_eq!(control.snapshot().snapshot.scenes, scenes);
    let scene_ids = [40, 41].map(|id| WireSceneId::new(NonZeroU128::new(id).unwrap()));
    assert_eq!(control.snapshot().snapshot.layout_scenes, scene_ids);
    let revision = control.snapshot().snapshot.revision;
    let initial = control.scene_animation_states();
    assert_eq!(
        initial,
        scenes
            .iter()
            .map(|input| SceneAnimationStateMessage {
                revision,
                input: *input,
                scene: scene_ids[0],
                animation_revision: 0,
                clip: Box::new(scene_clip_to_wire(&scene.animation.clip)),
            })
            .collect::<Vec<_>>()
    );
    let layouts = control.scene_layout_states();
    assert_eq!(
        layouts
            .iter()
            .map(|layout| (layout.scene, layout.name.as_str()))
            .collect::<Vec<_>>(),
        [(scene_ids[0], "Two up"), (scene_ids[1], "Spare wall")]
    );
    let layout = &layouts[0].layout;
    assert_eq!((layout.width, layout.height), (1_920, 1_080));
    assert_eq!(
        layout.layers[0],
        WireSceneLayerOutline {
            id: WireSceneLayerId::new(core::num::NonZeroU32::new(3).unwrap()),
            name: "Guest cam".into(),
            source: WireSceneLayerSource::Input(WireInputId::from_domain(input(1))),
            x: 100,
            y: 50,
            width: 640,
            height: 360,
            quarter_turns: 1,
            opacity: 200,
            enabled: true,
            tilt_millidegrees: 0,
            anchor_x: 5_000,
            anchor_y: 5_000,
            z_order: 0,
            crop: None,
            key: None,
            effects: Vec::new(),
            mask: None,
        }
    );
    assert_eq!(
        layouts[1].layout.layers[0].source,
        WireSceneLayerSource::Scene(scene_ids[0])
    );
    for message in layouts.into_iter().map(WireMessage::SceneLayoutState) {
        encode_line(&message).unwrap();
    }

    let scene_input = scenes[0];
    let layer = WireSceneLayerId::new(core::num::NonZeroU32::new(9).unwrap());
    let preset = CommandPayload::ApplySceneAnimationPreset {
        input: scene_input,
        base_revision: 0,
        layer,
        phase: Phase::In,
        preset: AnimationPreset::SlideFromLeft,
        frames: 12,
    };
    assert_eq!(
        control.required_classes(&preset).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            command("o", "o", preset.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    let applied = control
        .submit(&principal(Role::Graphics), command("p", "p", preset), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    let mut expected = scene.clone();
    expected
        .apply_animation_preset(
            fm_model::LayerId::new(9).unwrap(),
            Phase::In,
            AnimationPreset::SlideFromLeft,
            12,
            1_920,
            1_080,
        )
        .unwrap();
    let edited_clip = Box::new(scene_clip_to_wire(&expected.animation.clip));
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::SceneAnimationChanged {
            inputs: scenes.to_vec(),
            animation_revision: 1,
            clip: edited_clip.clone(),
        }
    );
    assert_wire_exact(&control, &applied);
    let states = control.scene_animation_states();
    assert!(states.iter().all(|state| state.clip == edited_clip));

    // A stale base revision is a conflict, through either Scene input.
    let stale = control
        .submit(
            &principal(Role::Graphics),
            command(
                "s",
                "s",
                CommandPayload::StretchSceneAnimation {
                    input: scenes[1],
                    base_revision: 0,
                    phase: Phase::In,
                    frames: 24,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&stale), Some("conflict"));
    assert_eq!(rejection_field_code(&stale), Some("revision_conflict"));

    // An unknown layer is refused.
    let unknown = control
        .submit(
            &principal(Role::Graphics),
            command(
                "u",
                "u",
                CommandPayload::ApplySceneAnimationPreset {
                    input: scene_input,
                    base_revision: 1,
                    layer: WireSceneLayerId::new(core::num::NonZeroU32::new(5).unwrap()),
                    phase: Phase::Out,
                    preset: AnimationPreset::Fade,
                    frames: 6,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));

    // Undo journals the restored clip as a revision-checked set.
    let message = undo("undo", None);
    let prepared = prepared(
        control
            .prepare_submit(&principal(Role::Graphics), message.clone(), 0)
            .unwrap(),
    );
    assert_eq!(
        prepared.replay_command(&message).payload,
        CommandPayload::SetSceneAnimation {
            input: scene_input,
            base_revision: 1,
            clip: Box::new(scene_clip_to_wire(&scene.animation.clip)),
        }
    );
    let undone = prepared.commit().unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        undone.output.events[0].payload,
        EventPayload::SceneAnimationChanged {
            inputs: scenes.to_vec(),
            animation_revision: 2,
            clip: Box::new(scene_clip_to_wire(&scene.animation.clip)),
        }
    );
    let redone = control
        .submit(&principal(Role::Graphics), redo("redo", None), 0)
        .unwrap();
    assert!(redone.is_accepted(), "{:?}", redone.output.result);
    let states = control.scene_animation_states();
    assert_eq!(states[0].animation_revision, 3);
    assert_eq!(states[0].clip, edited_clip);
}

/// GX-011 stage 5: a switch that takes an animated scene to Preview or
/// Program needs exactly the frames that play its phase out
/// ([`PreparedSubmission::settle_ticks`]); a switch without scenes in
/// motion needs only its own frames.
#[test]
fn scene_settle_ticks_cover_exactly_the_phases_a_switch_starts() {
    use fm_model::fm_animation::{AnimationPreset, Phase};
    use fm_protocol::WireSceneLayerId;

    let (mut control, _) = scene_control();
    let server = server_identity();
    let edited = control
        .submit(
            &principal(Role::Graphics),
            command(
                "p",
                "p",
                CommandPayload::ApplySceneAnimationPreset {
                    input: WireInputId::from_domain(input(4)),
                    base_revision: 0,
                    layer: WireSceneLayerId::new(core::num::NonZeroU32::new(9).unwrap()),
                    phase: Phase::In,
                    preset: AnimationPreset::Fade,
                    frames: 12,
                },
            ),
            0,
        )
        .unwrap();
    assert!(edited.is_accepted());
    control.tick(&server).unwrap();
    // 12 frames of the 30/1 clip are 24 frames at 60 fps.
    assert_eq!(control.engine.scene_phase_frames(input(4), Phase::In), 24);

    let select = command(
        "v",
        "v",
        CommandPayload::SelectPreview {
            input: WireInputId::from_domain(input(4)),
        },
    );
    let ticks = {
        let prepared = prepared(
            control
                .prepare_submit(&principal(Role::Operator), select, 0)
                .unwrap(),
        );
        let ticks = prepared.settle_ticks(1);
        prepared.commit().unwrap();
        ticks
    };
    assert!(ticks > 1, "{ticks}");
    for _ in 1..ticks {
        control.tick(&server).unwrap();
    }
    assert!(control.scenes_in_motion());
    control.tick(&server).unwrap();
    assert!(!control.scenes_in_motion());

    // Nothing animates: a switch needs only its own frame.
    let back = command(
        "b",
        "b",
        CommandPayload::SelectPreview {
            input: WireInputId::from_domain(input(2)),
        },
    );
    let prepared = prepared(
        control
            .prepare_submit(&principal(Role::Operator), back, 0)
            .unwrap(),
    );
    assert_eq!(prepared.settle_ticks(1), 1);
}

/// SW-019, Protocol 2.35: a scene layer grade is an `EditProject` command
/// addressed by scene; its `grade_changed` event and `grade_state` record
/// name the scene, a scene no Scene input shows is graded the same way, and
/// undo clears it.
#[test]
#[allow(clippy::too_many_lines)] // one golden: set, replicate, refuse, unshown, undo
fn scene_layer_grades_are_addressed_and_replicated_by_scene() {
    use fm_protocol::{GradeChange, GradeEntry, WireGradeSource, WireSceneId, WireSceneLayerId};

    let (mut control, _) = scene_control();
    let shared = WireSceneId::new(NonZeroU128::new(40).unwrap());
    let spare = WireSceneId::new(NonZeroU128::new(41).unwrap());
    let layer = WireSceneLayerId::new(core::num::NonZeroU32::new(9).unwrap());
    let warm = Box::new(fm_grade::builtin_preset("warm").unwrap().grade.clone());
    let set = CommandPayload::SetLayerGrade {
        scene: shared,
        layer,
        source: Some(WireGradeSource::Inline(warm.clone())),
        gesture: None,
    };
    assert_eq!(
        control.required_classes(&set).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(&principal(Role::Viewer), command("v", "v", set.clone()), 0)
        .unwrap();
    assert!(!denied.is_accepted());
    let accepted = control
        .submit(&principal(Role::Graphics), command("s", "s", set), 0)
        .unwrap();
    assert!(accepted.is_accepted(), "{:?}", accepted.output.result);
    assert_eq!(
        accepted.output.events[0].payload,
        EventPayload::GradeChanged(GradeChange::Layer {
            scene: shared,
            layer,
            source: Some(WireGradeSource::Inline(warm.clone())),
        })
    );
    assert_eq!(control.snapshot().snapshot.grade_states, 1);
    assert_eq!(
        control
            .grade_states()
            .into_iter()
            .map(|state| state.entry)
            .collect::<Vec<_>>(),
        [GradeEntry::Layer {
            scene: shared,
            layer,
            source: WireGradeSource::Inline(warm.clone()),
        }]
    );
    let compiled = control.compiled_grades().unwrap();
    assert!(
        compiled
            .layer(fm_model::SceneLayerRef::new(
                fm_types::SceneId::new(NonZeroU128::new(40).unwrap()),
                fm_model::LayerId::new(9).unwrap()
            ))
            .is_some()
    );

    // An unknown layer is refused.
    let unknown = control
        .submit(
            &principal(Role::Graphics),
            command(
                "x",
                "x",
                CommandPayload::SetLayerGrade {
                    scene: shared,
                    layer: WireSceneLayerId::new(core::num::NonZeroU32::new(2).unwrap()),
                    source: None,
                    gesture: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(!unknown.is_accepted());

    // Scene 41 has no Scene input; its layer is graded and replicated too.
    let nested = WireSceneLayerId::new(core::num::NonZeroU32::new(2).unwrap());
    let spare_set = control
        .submit(
            &principal(Role::Graphics),
            command(
                "n",
                "n",
                CommandPayload::SetLayerGrade {
                    scene: spare,
                    layer: nested,
                    source: Some(WireGradeSource::Inline(warm.clone())),
                    gesture: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(spare_set.is_accepted(), "{:?}", spare_set.output.result);
    assert_eq!(
        spare_set.output.events[0].payload,
        EventPayload::GradeChanged(GradeChange::Layer {
            scene: spare,
            layer: nested,
            source: Some(WireGradeSource::Inline(warm.clone())),
        })
    );
    assert_eq!(control.snapshot().snapshot.grade_states, 2);
    assert_eq!(
        control.grade_states()[1].entry,
        GradeEntry::Layer {
            scene: spare,
            layer: nested,
            source: WireGradeSource::Inline(warm),
        }
    );
    let undone = control
        .submit(
            &principal(Role::Graphics),
            command(
                "un",
                "un",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);

    let undone = control
        .submit(
            &principal(Role::Graphics),
            command(
                "u",
                "u",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        undone.output.events[0].payload,
        EventPayload::GradeChanged(GradeChange::Layer {
            scene: shared,
            layer,
            source: None,
        })
    );
    assert_eq!(control.snapshot().snapshot.grade_states, 0);
}

/// GX-011, Protocol 2.34: the animation preset library is edited by
/// `EditProject` commands that publish `animation_preset_changed`, follow
/// snapshots as `animation_preset_state` records, and undo through the
/// shared history; a library preset applies to a scene layer by id.
#[test]
#[allow(clippy::too_many_lines)] // one golden: add, apply, rename, remove, undo
fn animation_preset_library_edits_are_project_edits_with_state_records_and_undo() {
    use fm_model::fm_animation::{
        Easing, Keyframe, MotionClip, MotionTarget, Phase, PhaseMotion, Property, Timebase, Track,
        TrackKey, Value,
    };
    use fm_protocol::{AnimationPresetChange, AnimationPresetEntry, WireSceneLayerId};

    let (mut control, scene) = scene_control();
    let scene_input = control.snapshot().snapshot.scenes[0];
    let id = |value| core::num::NonZeroU32::new(value).unwrap();
    let clip = MotionClip::from_tracks(
        Timebase::new(fm_types::FrameRate::new(30, 1).unwrap()),
        10,
        0,
        vec![Track::new(
            TrackKey::new(MotionTarget, Phase::In, Property::PositionX),
            vec![
                Keyframe::new(0, Value::Scalar(-200), Easing::CubicOut),
                Keyframe::new(10, Value::Scalar(0), Easing::Linear),
            ],
        )],
    )
    .unwrap();
    let motion = PhaseMotion::new(clip, 640, 360).unwrap();
    let entry = |name: &str| AnimationPresetEntry {
        preset: id(1),
        name: name.into(),
        phase: Phase::In,
        motion: Box::new(motion.clone()),
    };
    let add = CommandPayload::AddAnimationPreset {
        preset: id(1),
        name: "Swoosh".into(),
        phase: Phase::In,
        motion: Box::new(motion.clone()),
        index: None,
    };
    let rename = CommandPayload::RenameAnimationPreset {
        preset: id(1),
        name: "Whoosh".into(),
    };
    let remove = CommandPayload::RemoveAnimationPreset { preset: id(1) };
    for payload in [&add, &rename, &remove] {
        assert_eq!(
            control.required_classes(payload).primary(),
            CommandClass::EditProject
        );
    }
    let denied = control
        .submit(
            &principal(Role::Operator),
            command("o", "o", add.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    assert_eq!(control.snapshot().snapshot.animation_preset_states, 0);

    let added = control
        .submit(
            &principal(Role::Graphics),
            command("a", "a", add.clone()),
            0,
        )
        .unwrap();
    assert!(added.is_accepted(), "{:?}", added.output.result);
    assert_eq!(
        added.output.events[0].payload,
        EventPayload::AnimationPresetChanged(AnimationPresetChange::Set {
            index: 0,
            entry: entry("Swoosh"),
        })
    );
    assert_wire_exact(&control, &added);
    assert_eq!(control.snapshot().snapshot.animation_preset_states, 1);
    let states = control.animation_preset_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].entry, entry("Swoosh"));
    assert_eq!(states[0].revision, control.snapshot().snapshot.revision);

    // The same id again is refused; so is an unknown preset applied.
    let again = control
        .submit(&principal(Role::Graphics), command("b", "b", add), 0)
        .unwrap();
    assert!(!again.is_accepted());
    let apply = |preset| CommandPayload::ApplySceneAnimationUserPreset {
        input: scene_input,
        base_revision: 0,
        layer: WireSceneLayerId::new(id(9)),
        phase: Phase::In,
        preset: id(preset),
        frames: 20,
    };
    let unknown = control
        .submit(&principal(Role::Graphics), command("u", "u", apply(7)), 0)
        .unwrap();
    assert!(!unknown.is_accepted());

    // Applied to layer 9: the scene's clip is the model's application.
    let applied = control
        .submit(&principal(Role::Graphics), command("p", "p", apply(1)), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    let mut expected = scene.clone();
    expected
        .apply_animation_motion(fm_model::LayerId::new(9).unwrap(), Phase::In, &motion, 20)
        .unwrap();
    assert_eq!(
        control.scene_animation_states()[0].clip,
        Box::new(scene_clip_to_wire(&expected.animation.clip))
    );
    assert_wire_exact(&control, &applied);

    let renamed = control
        .submit(&principal(Role::Graphics), command("r", "r", rename), 0)
        .unwrap();
    assert!(renamed.is_accepted(), "{:?}", renamed.output.result);
    assert_eq!(
        renamed.output.events[0].payload,
        EventPayload::AnimationPresetChanged(AnimationPresetChange::Set {
            index: 0,
            entry: entry("Whoosh"),
        })
    );
    let removed = control
        .submit(&principal(Role::Graphics), command("d", "d", remove), 0)
        .unwrap();
    assert!(removed.is_accepted(), "{:?}", removed.output.result);
    assert_eq!(
        removed.output.events[0].payload,
        EventPayload::AnimationPresetChanged(AnimationPresetChange::Removed { preset: id(1) })
    );
    assert_eq!(control.snapshot().snapshot.animation_preset_states, 0);

    // Undo brings the renamed preset back at its index.
    let undo = CommandPayload::Undo {
        expected_edit: None,
    };
    assert_eq!(
        control.required_classes(&undo).additional(),
        [CommandClass::EditProject]
    );
    let undone = control
        .submit(&principal(Role::Graphics), command("z", "z", undo), 0)
        .unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        undone.output.events[0].payload,
        EventPayload::AnimationPresetChanged(AnimationPresetChange::Set {
            index: 0,
            entry: entry("Whoosh"),
        })
    );
    assert_eq!(control.animation_preset_states()[0].entry, entry("Whoosh"));
}

/// SW-002, Protocol 2.36: every automatic mix is taken by kind with the
/// Transition class, and the favourite transition buttons are assigned by
/// an operator, replicated in the snapshot and a `transition_favorite_changed`
/// event, and undone exactly.
#[test]
fn transitions_by_kind_and_favourites_propagate_exactly() {
    use fm_protocol::{AutoTransitionKind, TransitionFavoriteEffect, TransitionFavoriteStatus};

    let mut control = service(64, 64);
    for (index, kind) in AutoTransitionKind::ALL.into_iter().enumerate() {
        let payload = CommandPayload::Transition {
            kind,
            duration_frames: 2,
        };
        assert_eq!(
            control.required_classes(&payload).primary(),
            CommandClass::Transition
        );
        let key = format!("take-{index}");
        let taken = control
            .submit(&principal(Role::Operator), command(&key, &key, payload), 0)
            .unwrap();
        assert!(taken.is_accepted(), "{kind:?}: {:?}", taken.output.result);
        assert_wire_exact(&control, &taken);
        let tick = control.tick(&server_identity()).unwrap();
        assert_eq!(
            tick.frame.program.transition_kind,
            Some(transition::domain_transition_kind(kind))
        );
        control.tick(&server_identity()).unwrap();
        control.tick(&server_identity()).unwrap();
    }

    assert_eq!(
        control.snapshot().snapshot.transition_favorites,
        TransitionFavoriteStatus::defaults()
    );
    let favorite = TransitionFavoriteStatus {
        effect: TransitionFavoriteEffect::Stinger(WireStingerSlotId::new(2).unwrap()),
        duration_frames: 45,
    };
    let assign = CommandPayload::SetTransitionFavorite { slot: 2, favorite };
    assert_eq!(
        control.required_classes(&assign).primary(),
        CommandClass::Transition
    );
    let denied = control
        .submit(
            &principal(Role::Viewer),
            command("v", "v", assign.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    let assigned = control
        .submit(&principal(Role::Operator), command("f", "f", assign), 0)
        .unwrap();
    assert!(assigned.is_accepted(), "{:?}", assigned.output.result);
    assert_eq!(
        assigned.output.events[0].payload,
        EventPayload::TransitionFavoriteChanged { slot: 2, favorite }
    );
    assert_wire_exact(&control, &assigned);
    assert_eq!(
        control.snapshot().snapshot.transition_favorites[1],
        favorite
    );

    // Undoing it needs both edit and switching rights.
    let undone = control
        .submit(
            &principal(Role::Admin),
            command(
                "u",
                "u",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        undone.output.events[0].payload,
        EventPayload::TransitionFavoriteChanged {
            slot: 2,
            favorite: TransitionFavoriteStatus::defaults()[1],
        }
    );
    assert_eq!(
        control.snapshot().snapshot.transition_favorites,
        TransitionFavoriteStatus::defaults()
    );
}

/// SW-005, Protocol 2.36: an overlay channel configured with a motion
/// transition replicates it and takes the configured frames to go on air.
#[test]
fn overlay_motion_transitions_replicate_and_take_their_duration() {
    use fm_protocol::{OverlayTransitionKind, WireOverlayChannelId};

    let mut control = service(64, 64);
    let channel = WireOverlayChannelId::new(5).unwrap();
    let configured = control
        .submit(
            &principal(Role::Operator),
            command(
                "c",
                "c",
                CommandPayload::ConfigureOverlayTransition {
                    channel,
                    transition: OverlayTransitionKind::CubeZoom,
                    duration_frames: 7,
                },
            ),
            0,
        )
        .unwrap();
    assert!(configured.is_accepted(), "{:?}", configured.output.result);
    assert_wire_exact(&control, &configured);
    assert_eq!(
        control.snapshot().snapshot.desired_overlays[4].transition,
        OverlayTransitionKind::CubeZoom
    );
    let prepared = prepared(
        control
            .prepare_submit(
                &principal(Role::Operator),
                command(
                    "t",
                    "t",
                    CommandPayload::TakeOverlay {
                        channel,
                        source: WireInputId::from_domain(input(3)),
                    },
                ),
                0,
            )
            .unwrap(),
    );
    assert_eq!(
        prepared.overlay_transition_ticks(fm_switcher::OverlayChannelId::new(5).unwrap()),
        7
    );
}

fn media_service() -> ControlService {
    let mut show = ShowState::new(
        "media show",
        named_inputs([input(1), input(2)]),
        input(1),
        input(2),
    )
    .unwrap();
    show.set_media_input(input(2), fm_model::MediaPlaybackSettings::default())
        .unwrap();
    let rate = FrameRate::new(25, 1).unwrap();
    let mut engine = Engine::new(show, rate, ClockDomainId::new(NonZeroU128::new(1).unwrap()));
    engine
        .set_media_source(
            input(2),
            Some(fm_engine::EngineMediaSource::synthetic(
                &fm_model::MediaPlaybackSettings::default(),
                rate,
            )),
        )
        .unwrap();
    ControlService::new(
        engine,
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 64,
            max_subscribers: 4,
            subscriber_queue: 256,
        },
    )
}

/// IN-003, Protocol 2.37: media transport is an operator control, media
/// settings are project edits; accepted commands publish `media_changed`
/// with the projected status, snapshots announce one `media_state` per media
/// input, and ticks publish `media` runtime events for what happened plus a
/// position sample each second.
#[test]
#[allow(clippy::too_many_lines)]
fn media_commands_states_and_runtime_events_propagate_exactly() {
    use fm_protocol::{
        MediaEndAction, MediaEvent, MediaOperation, MediaSeekTarget, MediaTransportState,
    };

    let mut control = media_service();
    let clip = WireInputId::from_domain(input(2));
    assert_eq!(control.snapshot().snapshot.media_states, 1);
    let states = control.media_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].status.input, clip);
    assert!(states[0].status.transport.available);
    assert_eq!(states[0].status.transport.frames, 250);

    let play = CommandPayload::Media {
        input: clip,
        operation: MediaOperation::Play,
    };
    assert_eq!(
        control.required_classes(&play).primary(),
        CommandClass::Transition
    );
    let loop_end = CommandPayload::Media {
        input: clip,
        operation: MediaOperation::EndAction(MediaEndAction::Loop),
    };
    assert_eq!(
        control.required_classes(&loop_end).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            command("o", "o", loop_end.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    let looped = control
        .submit(&principal(Role::Graphics), command("l", "l", loop_end), 0)
        .unwrap();
    assert!(looped.is_accepted(), "{:?}", looped.output.result);
    assert_wire_exact(&control, &looped);
    control.tick(&server_identity()).unwrap();

    let subscription = control.subscribe().unwrap();
    let played = control
        .submit(&principal(Role::Operator), command("p", "p", play), 0)
        .unwrap();
    assert!(played.is_accepted(), "{:?}", played.output.result);
    assert_wire_exact(&control, &played);
    let EventPayload::MediaChanged {
        frame,
        operation,
        status,
    } = &played.output.events[0].payload
    else {
        panic!("{:?}", played.output.events[0].payload)
    };
    assert_eq!((*frame, *operation), (1, MediaOperation::Play));
    assert_eq!(status.transport.state, MediaTransportState::Playing);
    assert_eq!(status.settings.end_action, MediaEndAction::Loop);

    // The first frame publishes the realized command and the state change;
    // then a position sample each second.
    let mut media_events = Vec::new();
    for _ in 0..60 {
        let tick = control.tick(&server_identity()).unwrap();
        for event in tick.runtime_events {
            let message = WireMessage::RuntimeEvent(event.clone());
            let line = fm_protocol::encode_line(&message).unwrap();
            assert_eq!(fm_protocol::decode_line(&line).unwrap(), message);
            if let RuntimeLifecycleEvent::Media { frame, status, .. } = event.event {
                media_events.push((frame, status.transport.position));
            }
        }
    }
    assert_eq!(media_events, [(1, 1), (25, 25), (50, 50)]);
    let mut received = 0;
    while let Ok(event) = subscription.try_recv() {
        if matches!(
            event,
            LiveEvent::Runtime(RuntimeEventMessage {
                event: RuntimeLifecycleEvent::Media { .. },
                ..
            })
        ) {
            received += 1;
        }
    }
    assert_eq!(received, 3);

    // Seeks near the end fire the configured loop.
    let seek = control
        .submit(
            &principal(Role::Operator),
            command(
                "s",
                "s",
                CommandPayload::Media {
                    input: clip,
                    operation: MediaOperation::Seek(MediaSeekTarget::Frame(248)),
                },
            ),
            0,
        )
        .unwrap();
    assert!(seek.is_accepted(), "{:?}", seek.output.result);
    let mut events = Vec::new();
    for _ in 0..3 {
        for event in control.tick(&server_identity()).unwrap().runtime_events {
            if let RuntimeLifecycleEvent::Media { events: list, .. } = event.event {
                events.extend(list);
            }
        }
    }
    assert!(events.contains(&MediaEvent::Seeked { frame: 248 }));
    assert!(events.contains(&MediaEvent::Ended {
        action: MediaEndAction::Loop
    }));
    assert!(events.contains(&MediaEvent::Looped { count: 1 }));

    // A non-media input refuses media commands.
    let refused = control
        .submit(
            &principal(Role::Operator),
            command(
                "x",
                "x",
                CommandPayload::Media {
                    input: WireInputId::from_domain(input(1)),
                    operation: MediaOperation::Play,
                },
            ),
            0,
        )
        .unwrap();
    assert!(!refused.is_accepted());

    // A fault is published once and replicated in the state records.
    control
        .set_media_fault(input(2), fm_engine::EngineMediaFault::DecodeFailed)
        .unwrap();
    let tick = control.tick(&server_identity()).unwrap();
    assert!(tick.runtime_events.iter().any(|event| matches!(
        &event.event,
        RuntimeLifecycleEvent::Media { events, .. }
            if events == &[MediaEvent::Faulted { fault: fm_protocol::MediaFault::DecodeFailed }]
    )));
    assert_eq!(
        control.media_states()[0].status.transport.fault,
        Some(fm_protocol::MediaFault::DecodeFailed)
    );
}

/// IN-026, Protocol 2.38: a media fault takes its input offline in the
/// snapshot with one `input_availability` runtime event, a source that
/// plays again brings it back, the media plane may report availability
/// directly, and lifecycle commands are project edits replicated in the
/// snapshot's input lifecycle and category list.
#[test]
#[allow(clippy::too_many_lines)] // one scenario from fault to replicated edits
fn media_faults_take_inputs_offline_and_lifecycle_edits_replicate() {
    let mut control = media_service();
    let availability = |events: &[RuntimeEventMessage]| {
        events
            .iter()
            .filter_map(|event| match &event.event {
                RuntimeLifecycleEvent::InputAvailability { input, offline } => {
                    Some((*input, *offline))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let lifecycle = |control: &ControlService, value: u128| {
        control
            .snapshot()
            .snapshot
            .inputs
            .iter()
            .find(|status| status.input == WireInputId::from_domain(input(value)))
            .unwrap()
            .lifecycle
    };
    control
        .set_media_fault(input(2), fm_engine::EngineMediaFault::SourceMissing)
        .unwrap();
    let tick = control.tick(&server_identity()).unwrap();
    assert_eq!(
        availability(&tick.runtime_events),
        [(
            WireInputId::from_domain(input(2)),
            Some(fm_protocol::InputOfflineReason::SourceMissing)
        )]
    );
    assert_eq!(
        lifecycle(&control, 2).offline,
        Some(fm_protocol::InputOfflineReason::SourceMissing)
    );
    let tick = control.tick(&server_identity()).unwrap();
    assert_eq!(availability(&tick.runtime_events), Vec::new());

    // The source opens again and plays: the input is back.
    control
        .set_media_source(
            input(2),
            Some(fm_engine::EngineMediaSource::synthetic(
                &fm_model::MediaPlaybackSettings::default(),
                FrameRate::new(25, 1).unwrap(),
            )),
        )
        .unwrap();
    let tick = control.tick(&server_identity()).unwrap();
    assert_eq!(
        availability(&tick.runtime_events),
        [(WireInputId::from_domain(input(2)), None)]
    );
    assert_eq!(lifecycle(&control, 2).offline, None);

    // The media plane reports a lost device directly, once.
    let event = control
        .set_input_offline(
            &server_identity(),
            input(1),
            Some(fm_protocol::InputOfflineReason::DeviceUnavailable),
        )
        .unwrap();
    assert!(event.is_some());
    assert!(
        control
            .set_input_offline(
                &server_identity(),
                input(1),
                Some(fm_protocol::InputOfflineReason::DeviceUnavailable),
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        lifecycle(&control, 1).offline,
        Some(fm_protocol::InputOfflineReason::DeviceUnavailable)
    );

    // Lifecycle commands are project edits replicated in the snapshot.
    for (key, operation) in [
        (
            "categories",
            fm_protocol::InputLifecycleOperation::SetCategories {
                categories: vec![fm_protocol::InputCategoryStatus {
                    id: 3,
                    name: "Clips".into(),
                }],
            },
        ),
        (
            "category",
            fm_protocol::InputLifecycleOperation::SetCategory {
                input: WireInputId::from_domain(input(2)),
                category: Some(3),
            },
        ),
        (
            "colour",
            fm_protocol::InputLifecycleOperation::SetColor {
                input: WireInputId::from_domain(input(2)),
                color: Some(fm_protocol::InputColorLabel::Yellow),
            },
        ),
        (
            "pause",
            fm_protocol::InputLifecycleOperation::SetPaused {
                input: WireInputId::from_domain(input(2)),
                paused: true,
            },
        ),
    ] {
        let viewer = control
            .submit(
                &principal(Role::Viewer),
                command(key, key, CommandPayload::InputLifecycle(operation.clone())),
                0,
            )
            .unwrap();
        assert!(!viewer.is_accepted(), "{key}");
        let accepted = control
            .submit(
                &principal(Role::Admin),
                command(
                    &format!("{key}-admin"),
                    &format!("{key}-admin"),
                    CommandPayload::InputLifecycle(operation),
                ),
                0,
            )
            .unwrap();
        assert!(accepted.is_accepted(), "{key}");
        control.tick(&server_identity()).unwrap();
    }
    assert_eq!(
        control.snapshot().snapshot.input_categories,
        [fm_protocol::InputCategoryStatus {
            id: 3,
            name: "Clips".into(),
        }]
    );
    let two = lifecycle(&control, 2);
    assert_eq!(
        (two.category, two.color, two.paused, two.closed),
        (
            Some(3),
            Some(fm_protocol::InputColorLabel::Yellow),
            true,
            false
        )
    );
}

/// SW-006, Protocol 2.39: a live scene layer edit is an `EditProject`
/// command whose `scene_layout_changed` event carries the edit and the
/// scene's whole new layout (crop, mask, z-order, tilt, anchor), the
/// layout records follow it, a gesture's continuing edits fold into one
/// undo step, and undo journals the exact inverse edit.
#[test]
#[allow(clippy::too_many_lines)]
fn scene_layer_edits_replicate_the_whole_layout_and_undo_exactly() {
    use fm_protocol::{
        SceneLayerEdit, SceneLayerOperation, WireLayerCrop, WireLayerGeometry, WireSceneId,
        WireSceneLayerId,
    };

    let (mut control, scene) = scene_control();
    let scene_id = WireSceneId::new(NonZeroU128::new(40).unwrap());
    let host = WireSceneLayerId::new(core::num::NonZeroU32::new(9).unwrap());
    let geometry = |x: i32| WireLayerGeometry {
        x,
        y: 60,
        width: 640,
        height: 360,
        quarter_turns: 0,
        tilt_millidegrees: 17_500,
        anchor_x: 0,
        anchor_y: 10_000,
    };
    let edit = |operation: SceneLayerOperation, continues: bool| {
        CommandPayload::SceneLayer(SceneLayerEdit {
            scene: scene_id,
            layer: host,
            operation,
            continues,
            // Every edit of this drag belongs to one gesture.
            gesture: 11,
        })
    };
    let first = edit(SceneLayerOperation::SetGeometry(geometry(300)), false);
    assert_eq!(
        control.required_classes(&first).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            command("o", "o", first.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    let applied = control
        .submit(&principal(Role::Graphics), command("g1", "g1", first), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    let layout = control.scene_layout_states()[0].layout.clone();
    assert_eq!(layout.layers[1].geometry(), geometry(300));
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::SceneLayoutChanged {
            edit: SceneLayerEdit {
                scene: scene_id,
                layer: host,
                operation: SceneLayerOperation::SetGeometry(geometry(300)),
                continues: false,
                gesture: 11,
            },
            layout: layout.clone(),
        }
    );
    // The model the native renderer follows changed too.
    let model = control.scene_model(scene.id).unwrap();
    assert_eq!(model.layers[1].geometry.tilt_millidegrees, 17_500);

    // The drag continues: one undo step.
    for (index, x) in [320, 340].into_iter().enumerate() {
        let key = format!("g{}", index + 2);
        let continued = control
            .submit(
                &principal(Role::Graphics),
                command(
                    &key,
                    &key,
                    edit(SceneLayerOperation::SetGeometry(geometry(x)), true),
                ),
                0,
            )
            .unwrap();
        assert!(continued.is_accepted(), "{:?}", continued.output.result);
        assert_eq!(continued.output.events[0].edit_history.undo_depth, 1);
    }
    // A crop and a z-order replicate in the layout too.
    for (key, operation) in [
        (
            "crop",
            SceneLayerOperation::SetCrop(Some(WireLayerCrop {
                x: 10,
                y: 20,
                width: 1_000,
                height: 500,
            })),
        ),
        ("z", SceneLayerOperation::SetZOrder { z_order: -4 }),
    ] {
        let accepted = control
            .submit(
                &principal(Role::Graphics),
                command(key, key, edit(operation, false)),
                0,
            )
            .unwrap();
        assert!(accepted.is_accepted(), "{:?}", accepted.output.result);
        assert_wire_exact(&control, &accepted);
    }
    let layout = control.scene_layout_states()[0].layout.clone();
    let outline = &layout.layers[1];
    assert_eq!(outline.geometry(), geometry(340));
    assert_eq!(outline.z_order, -4);
    assert_eq!(
        outline.crop,
        Some(WireLayerCrop {
            x: 10,
            y: 20,
            width: 1_000,
            height: 500,
        })
    );

    // Undo journals the exact inverse as a live edit.
    for key in ["u1", "u2"] {
        let undone = control
            .submit(&principal(Role::Graphics), undo(key, None), 0)
            .unwrap();
        assert!(undone.is_accepted(), "{:?}", undone.output.result);
    }
    let message = undo("u3", None);
    let prepared = prepared(
        control
            .prepare_submit(&principal(Role::Graphics), message.clone(), 0)
            .unwrap(),
    );
    let original = &scene.layers[1].geometry;
    assert_eq!(
        prepared.replay_command(&message).payload,
        CommandPayload::SceneLayer(SceneLayerEdit {
            scene: scene_id,
            layer: host,
            operation: SceneLayerOperation::SetGeometry(WireLayerGeometry {
                x: original.translation_x,
                y: original.translation_y,
                width: original.width,
                height: original.height,
                quarter_turns: 0,
                tilt_millidegrees: 0,
                anchor_x: 5_000,
                anchor_y: 5_000,
            }),
            continues: false,
            gesture: 0,
        })
    );
    let undone = prepared.commit().unwrap();
    assert!(undone.is_accepted(), "{:?}", undone.output.result);
    assert_eq!(
        control.scene_model(scene.id).unwrap().layers[1].geometry,
        *original
    );

    // An edit of a scene the show does not have is refused.
    let unknown = control
        .submit(
            &principal(Role::Graphics),
            command(
                "x",
                "x",
                CommandPayload::SceneLayer(SceneLayerEdit {
                    scene: WireSceneId::new(NonZeroU128::new(99).unwrap()),
                    layer: host,
                    operation: SceneLayerOperation::Remove,
                    continues: false,
                    gesture: 0,
                }),
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));
}

fn mixer_service() -> ControlService {
    let show = ShowState::new(
        "show",
        named_inputs([input(1), input(2), input(3)]),
        input(1),
        input(2),
    )
    .unwrap()
    .with_outputs(vec![
        (output(7), "Program".to_owned()),
        (output(8), "Confidence".to_owned()),
    ])
    .unwrap();
    let engine = Engine::new(
        show,
        FrameRate::new(60, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(1).unwrap()),
    );
    ControlService::new(
        engine,
        Policy::production(),
        "engine-a",
        "log-a",
        ControlLimits {
            retained_events: 16,
            max_subscribers: 4,
            subscriber_queue: 16,
        },
    )
}

/// Protocol 2.40 (AU-001): strip labels, group membership, group names and
/// group levels are audio-controlled, replicated exactly in events and the
/// snapshot, undoable (except live levels), and realized on the frame.
#[test]
#[allow(clippy::too_many_lines)]
fn audio_mixer_edits_are_audio_controlled_replicated_and_undoable() {
    use fm_engine::{EngineAudioGroupState, EngineAudioMixerUpdate};
    use fm_protocol::AudioMixerOperation;

    let mut control = mixer_service();
    let server = server_identity();
    let label = CommandPayload::AudioMixer(AudioMixerOperation::SetLabel {
        input: WireInputId::from_domain(input(2)),
        label: "Guest mic".into(),
    });
    assert_eq!(
        control.required_classes(&label).primary(),
        CommandClass::ControlAudio
    );
    let denied = control
        .submit(
            &principal(Role::Graphics),
            command("g", "g", label.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    let applied = control
        .submit(&principal(Role::Audio), command("a1", "a1", label), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::AudioMixerChanged(AudioMixerOperation::SetLabel {
            input: WireInputId::from_domain(input(2)),
            label: "Guest mic".into(),
        })
    );
    for (key, operation) in [
        (
            "a2",
            AudioMixerOperation::SetGroup {
                input: WireInputId::from_domain(input(2)),
                group: Some(3),
            },
        ),
        (
            "a3",
            AudioMixerOperation::RenameGroup {
                group: 3,
                name: "Guests".into(),
            },
        ),
        (
            "a4",
            AudioMixerOperation::SetGroupLevel {
                group: 3,
                gain_millidb: -6_000,
                muted: true,
            },
        ),
    ] {
        let submission = control
            .submit(
                &principal(Role::Audio),
                command(key, key, CommandPayload::AudioMixer(operation.clone())),
                0,
            )
            .unwrap();
        assert!(submission.is_accepted(), "{key}");
        assert_wire_exact(&control, &submission);
        assert_eq!(
            submission.output.events[0].payload,
            EventPayload::AudioMixerChanged(operation)
        );
    }
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(
        snapshot
            .audio_mixer
            .label(WireInputId::from_domain(input(2))),
        "Guest mic"
    );
    assert_eq!(
        snapshot.audio_mixer.groups[2],
        fm_protocol::AudioGroupStatus {
            name: "Guests".into(),
            gain_millidb: -6_000,
            muted: true,
            soloed: false,
            parent: None,
        }
    );
    let strip = snapshot
        .input_audio_strips
        .iter()
        .find(|strip| strip.input == WireInputId::from_domain(input(2)))
        .unwrap();
    assert_eq!(strip.group, Some(3));

    let tick = control.tick(&server).unwrap();
    assert_eq!(
        tick.frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::Membership {
                input: input(2),
                group: fm_model::AudioGroupNumber::new(3),
            },
            EngineAudioMixerUpdate::GroupLevel {
                group: fm_model::AudioGroupNumber::new(3).unwrap(),
                state: EngineAudioGroupState {
                    gain_millidb: -6_000,
                    muted: true,
                },
            },
        ]
    );

    // Undo (which also needs EditProject) reverts the rename, then the
    // membership; the live level stays.
    for key in ["u1", "u2"] {
        let undo = control
            .submit(
                &principal(Role::Admin),
                command(
                    key,
                    key,
                    CommandPayload::Undo {
                        expected_edit: None,
                    },
                ),
                0,
            )
            .unwrap();
        assert!(undo.is_accepted(), "{:?}", undo.output.result);
        assert_wire_exact(&control, &undo);
    }
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(snapshot.audio_mixer.groups[2].name, "Group 3");
    assert!(snapshot.audio_mixer.groups[2].muted);
    assert!(
        snapshot
            .input_audio_strips
            .iter()
            .all(|strip| strip.group.is_none())
    );
}

/// Protocol 2.44 (AU-001): group solo is a live audio operation; the solo
/// mode and group parents are audio-controlled undoable project edits that
/// refuse cycles; all replicate exactly and reach the realized frame.
#[test]
#[allow(clippy::too_many_lines)]
fn group_solo_parents_and_solo_mode_are_replicated_and_realized() {
    use fm_engine::EngineAudioMixerUpdate;
    use fm_protocol::{AudioMixerOperation, AudioSoloMode};

    let mut control = mixer_service();
    let server = server_identity();
    let group = |number| fm_model::AudioGroupNumber::new(number).unwrap();
    for (key, operation) in [
        (
            "mode",
            AudioMixerOperation::SetSoloMode {
                mode: AudioSoloMode::Pfl,
            },
        ),
        (
            "parent",
            AudioMixerOperation::SetGroupParent {
                group: 1,
                parent: Some(2),
            },
        ),
        (
            "solo",
            AudioMixerOperation::SetGroupSolo {
                group: 2,
                soloed: true,
            },
        ),
    ] {
        let payload = CommandPayload::AudioMixer(operation.clone());
        assert_eq!(
            control.required_classes(&payload).primary(),
            CommandClass::ControlAudio,
            "{key}"
        );
        let denied = control
            .submit(
                &principal(Role::Graphics),
                command(&format!("g-{key}"), &format!("g-{key}"), payload.clone()),
                0,
            )
            .unwrap();
        assert_eq!(rejection_code(&denied), Some("permission_denied"));
        let submission = control
            .submit(&principal(Role::Audio), command(key, key, payload), 0)
            .unwrap();
        assert!(
            submission.is_accepted(),
            "{key}: {:?}",
            submission.output.result
        );
        assert_wire_exact(&control, &submission);
        assert_eq!(
            submission.output.events[0].payload,
            EventPayload::AudioMixerChanged(operation)
        );
    }
    let cycle = control
        .submit(
            &principal(Role::Audio),
            command(
                "cycle",
                "cycle",
                CommandPayload::AudioMixer(AudioMixerOperation::SetGroupParent {
                    group: 2,
                    parent: Some(1),
                }),
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&cycle), Some("invalid_command"));

    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(snapshot.audio_mixer.solo_mode, AudioSoloMode::Pfl);
    assert_eq!(snapshot.audio_mixer.groups[0].parent, Some(2));
    assert!(snapshot.audio_mixer.groups[1].soloed);
    assert!(snapshot.audio_mixer.group_chain_soloed(Some(1)));

    let tick = control.tick(&server).unwrap();
    assert_eq!(
        tick.frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::SoloMode(fm_model::AudioSoloMode::Pfl),
            EngineAudioMixerUpdate::GroupParent {
                group: group(1),
                parent: Some(group(2)),
            },
            EngineAudioMixerUpdate::GroupSolo {
                group: group(2),
                soloed: true,
            },
        ]
    );

    // Undo reverts the parent and then the mode; the live solo stays.
    for key in ["u1", "u2"] {
        let undo = control
            .submit(
                &principal(Role::Admin),
                command(
                    key,
                    key,
                    CommandPayload::Undo {
                        expected_edit: None,
                    },
                ),
                0,
            )
            .unwrap();
        assert!(undo.is_accepted(), "{:?}", undo.output.result);
        assert_wire_exact(&control, &undo);
    }
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(snapshot.audio_mixer.solo_mode, AudioSoloMode::InPlace);
    assert_eq!(snapshot.audio_mixer.groups[0].parent, None);
    assert!(snapshot.audio_mixer.groups[1].soloed);
}

/// Protocol 2.53 (AU-001): the headphone device is an audio-controlled
/// undoable project edit and its level a live audio operation; both
/// replicate exactly into the snapshot and reach the realized frame.
#[test]
fn the_monitor_output_is_replicated_and_realized() {
    use fm_engine::EngineAudioMixerUpdate;
    use fm_protocol::{AudioMixerOperation, AudioMonitorStatus};

    let mut control = mixer_service();
    let server = server_identity();
    for (key, operation) in [
        (
            "device",
            AudioMixerOperation::SetMonitorDevice {
                device: Some("default".into()),
            },
        ),
        (
            "level",
            AudioMixerOperation::SetMonitorLevel {
                gain_millidb: -3_000,
                muted: true,
            },
        ),
    ] {
        let payload = CommandPayload::AudioMixer(operation.clone());
        assert_eq!(
            control.required_classes(&payload).primary(),
            CommandClass::ControlAudio,
            "{key}"
        );
        let submission = control
            .submit(&principal(Role::Audio), command(key, key, payload), 0)
            .unwrap();
        assert!(
            submission.is_accepted(),
            "{key}: {:?}",
            submission.output.result
        );
        assert_wire_exact(&control, &submission);
        assert_eq!(
            submission.output.events[0].payload,
            EventPayload::AudioMixerChanged(operation)
        );
    }
    assert_eq!(
        control.snapshot().snapshot.audio_mixer.monitor,
        AudioMonitorStatus {
            device: Some("default".into()),
            gain_millidb: -3_000,
            muted: true,
        }
    );
    let tick = control.tick(&server).unwrap();
    assert_eq!(
        tick.frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::MonitorDevice(Some("default".into())),
            EngineAudioMixerUpdate::MonitorLevel(fm_engine::EngineAudioGroupState {
                gain_millidb: -3_000,
                muted: true,
            }),
        ]
    );
    let undo = control
        .submit(
            &principal(Role::Admin),
            command(
                "u",
                "u",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undo.is_accepted(), "{:?}", undo.output.result);
    assert_wire_exact(&control, &undo);
    let monitor = control.snapshot().snapshot.audio_mixer.monitor.clone();
    assert_eq!(monitor.device, None);
    assert!(monitor.muted);
}

/// Protocol 2.40 (SW-004): the Fade-to-Black output selection is a
/// switcher-operator project edit, replicated and undoable.
#[test]
fn fade_to_black_output_selection_is_operator_controlled_and_undoable() {
    let mut control = mixer_service();
    let payload = CommandPayload::SetFadeToBlackOutput {
        output: fm_protocol::WireOutputId::from_domain(output(8)),
        affected: false,
    };
    assert_eq!(
        control.required_classes(&payload).primary(),
        CommandClass::Transition
    );
    let denied = control
        .submit(
            &principal(Role::Audio),
            command("a", "a", payload.clone()),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));
    let applied = control
        .submit(&principal(Role::Operator), command("o", "o", payload), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::FadeToBlackOutputChanged {
            output: fm_protocol::WireOutputId::from_domain(output(8)),
            affected: false,
        }
    );
    assert_eq!(
        control.snapshot().snapshot.fade_to_black_excluded_outputs,
        vec![fm_protocol::WireOutputId::from_domain(output(8))]
    );
    let unknown = control
        .submit(
            &principal(Role::Operator),
            command(
                "x",
                "x",
                CommandPayload::SetFadeToBlackOutput {
                    output: fm_protocol::WireOutputId::from_domain(output(9)),
                    affected: false,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));
    let undo = control
        .submit(
            &principal(Role::Admin),
            command(
                "u",
                "u",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undo.is_accepted(), "{:?}", undo.output.result);
    assert_eq!(
        undo.output.events[0].payload,
        EventPayload::FadeToBlackOutputChanged {
            output: fm_protocol::WireOutputId::from_domain(output(8)),
            affected: true,
        }
    );
    assert_eq!(
        control.snapshot().snapshot.fade_to_black_excluded_outputs,
        Vec::new()
    );
}

/// Protocol 2.48 (SW-004): per-output Fade-to-Black durations and the
/// curve are switcher-operator project edits, replicated in the snapshot
/// and undoable; a manual Stinger replicates its clip length.
#[test]
#[allow(clippy::too_many_lines)]
fn fade_to_black_timing_and_manual_kinds_replicate_exactly() {
    let mut control = mixer_service();
    let wire = fm_protocol::WireOutputId::from_domain(output(8));
    let duration = CommandPayload::SetFadeToBlackOutputDuration {
        output: wire,
        duration_frames: Some(45),
    };
    let curve = CommandPayload::SetFadeToBlackCurve {
        curve: fm_protocol::FadeToBlackCurve::Ease,
    };
    for payload in [&duration, &curve] {
        assert_eq!(
            control.required_classes(payload).primary(),
            CommandClass::Transition
        );
        let denied = control
            .submit(
                &principal(Role::Audio),
                command("a", "a", payload.clone()),
                0,
            )
            .unwrap();
        assert_eq!(rejection_code(&denied), Some("permission_denied"));
    }
    let applied = control
        .submit(&principal(Role::Operator), command("d", "d", duration), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::FadeToBlackOutputDurationChanged {
            output: wire,
            duration_frames: Some(45),
        }
    );
    let applied = control
        .submit(&principal(Role::Operator), command("c", "c", curve), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::FadeToBlackCurveChanged {
            curve: fm_protocol::FadeToBlackCurve::Ease,
        }
    );
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(snapshot.fade_to_black_output_durations, vec![(wire, 45)]);
    assert_eq!(
        snapshot.fade_to_black_curve,
        fm_protocol::FadeToBlackCurve::Ease
    );
    for (key, payload, code) in [
        (
            "zero",
            CommandPayload::SetFadeToBlackOutputDuration {
                output: wire,
                duration_frames: Some(0),
            },
            "invalid_command",
        ),
        (
            "unknown",
            CommandPayload::SetFadeToBlackOutputDuration {
                output: fm_protocol::WireOutputId::from_domain(output(9)),
                duration_frames: Some(45),
            },
            "not_found",
        ),
    ] {
        let refused = control
            .submit(&principal(Role::Operator), command(key, key, payload), 0)
            .unwrap();
        assert_eq!(rejection_code(&refused), Some(code), "{key}");
    }
    for (key, expected) in [
        (
            "u1",
            EventPayload::FadeToBlackCurveChanged {
                curve: fm_protocol::FadeToBlackCurve::Linear,
            },
        ),
        (
            "u2",
            EventPayload::FadeToBlackOutputDurationChanged {
                output: wire,
                duration_frames: None,
            },
        ),
    ] {
        let undo = control
            .submit(
                &principal(Role::Admin),
                command(
                    key,
                    key,
                    CommandPayload::Undo {
                        expected_edit: None,
                    },
                ),
                0,
            )
            .unwrap();
        assert!(undo.is_accepted(), "{:?}", undo.output.result);
        assert_eq!(undo.output.events[0].payload, expected, "{key}");
    }
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(snapshot.fade_to_black_output_durations, Vec::new());
    assert_eq!(
        snapshot.fade_to_black_curve,
        fm_protocol::FadeToBlackCurve::Linear
    );

    // Every manual kind maps to the engine and back unchanged.
    let kinds = fm_protocol::AutoTransitionKind::ALL
        .map(ManualTransitionKind::from_mix)
        .into_iter()
        .chain([ManualTransitionKind::Stinger {
            slot: fm_protocol::WireStingerSlotId::new(4).unwrap(),
            duration_frames: 30,
        }]);
    for kind in kinds {
        assert_eq!(
            crate::wire_manual_kind(crate::engine_manual_kind(kind)),
            kind
        );
    }
}

#[allow(clippy::unnecessary_box_returns)] // every caller wants the box
fn display_config(text: &str) -> Box<fm_protocol::DisplayOutputConfig> {
    Box::new(text.parse().unwrap())
}

fn display_slot(slot: u8) -> fm_protocol::DisplayOutputSlot {
    fm_protocol::DisplayOutputSlot::new(slot).unwrap()
}

/// Protocol 2.42 (OR-011): display-output slots are padlocked, undoable
/// project edits replicated in the snapshot, and the presenter's per-slot
/// state and inventory are runtime events.
#[test]
#[allow(clippy::too_many_lines)] // one scenario from edit to runtime state
fn display_outputs_are_undoable_project_edits_with_runtime_state() {
    let mut control = mixer_service();
    let set = |id: &str, slot: u8, text: &str| {
        command(
            id,
            &format!("{id}-key"),
            CommandPayload::SetDisplayOutput {
                slot: display_slot(slot),
                config: display_config(text),
            },
        )
    };
    let payload = CommandPayload::ClearDisplayOutput {
        slot: display_slot(1),
    };
    assert_eq!(
        control.required_classes(&payload).primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            set("op", 1, "display=index:1,source=program"),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));

    let admin = principal(Role::Admin);
    let applied = control
        .submit(&admin, set("one", 1, "display=index:1,source=output:8"), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::DisplayOutputChanged {
            slot: display_slot(1),
            config: Some(display_config("display=index:1,source=output:8")),
        }
    );
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(
        snapshot.display_outputs,
        vec![fm_protocol::DisplayOutputEntry {
            slot: display_slot(1),
            config: *display_config("display=index:1,source=output:8"),
        }]
    );
    // No presenter has reported yet.
    assert_eq!(
        snapshot.display_output_states,
        vec![fm_protocol::DisplayOutputStatus {
            slot: display_slot(1),
            state: fm_protocol::WireDisplayOutputState::Waiting(
                fm_protocol::WaitReason::NoPresenter
            ),
        }]
    );

    // The same display in another slot, and an unknown output, are refused.
    let in_use = control
        .submit(&admin, set("two", 2, "display=index:1,source=program"), 0)
        .unwrap();
    assert_eq!(rejection_code(&in_use), Some("conflict"));
    let unknown = control
        .submit(
            &admin,
            set("three", 2, "display=index:2,source=output:9"),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));

    // The presenter reports the slot active, once.
    let active = fm_protocol::WireDisplayOutputState::Active(fm_protocol::WireActiveDisplay {
        id: fm_protocol::DisplayId::new("fake-1").unwrap(),
        name: "Fake 1".to_owned(),
        mode: "1920x1080@60000".parse().unwrap(),
        mode_fallback: false,
        hdr: fm_protocol::HdrState::Sdr,
    });
    let event = control
        .set_display_output_status(&server_identity(), display_slot(1), active.clone())
        .unwrap()
        .expect("a changed state is published");
    assert!(matches!(
        &event.event,
        RuntimeLifecycleEvent::DisplayOutputStatus(status)
            if status.slot == display_slot(1) && status.state == active
    ));
    let line = fm_protocol::encode_line(&WireMessage::RuntimeEvent(event.clone())).unwrap();
    assert_eq!(
        fm_protocol::decode_line(&line).unwrap(),
        WireMessage::RuntimeEvent(event)
    );
    assert!(
        control
            .set_display_output_status(&server_identity(), display_slot(1), active.clone())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        control.snapshot().snapshot.display_output_states[0].state,
        active
    );
    // An unconfigured slot's state is kept but not published.
    assert!(
        control
            .set_display_output_status(
                &server_identity(),
                display_slot(2),
                fm_protocol::WireDisplayOutputState::Waiting(
                    fm_protocol::WaitReason::DisplayAbsent
                ),
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(control.snapshot().snapshot.display_output_states.len(), 1);

    let inventory = fm_protocol::DisplayInventory {
        presenter: true,
        error: None,
        displays: Vec::new(),
    };
    assert!(
        control
            .set_display_inventory(&server_identity(), inventory.clone())
            .unwrap()
            .is_some()
    );
    assert!(
        control
            .set_display_inventory(&server_identity(), inventory.clone())
            .unwrap()
            .is_none()
    );
    assert_eq!(control.display_inventory(), &inventory);

    // The project padlock refuses the edit; undo restores the slot.
    let locked = control
        .submit(
            &admin,
            lock("lock", fm_protocol::EditLockScope::Project, true),
            0,
        )
        .unwrap();
    assert!(locked.is_accepted(), "{:?}", locked.output.result);
    let refused = control
        .submit(&admin, command("clear", "clear-key", payload.clone()), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("project_locked"));
    let unlocked = control
        .submit(
            &admin,
            lock("unlock", fm_protocol::EditLockScope::Project, false),
            0,
        )
        .unwrap();
    assert!(unlocked.is_accepted(), "{:?}", unlocked.output.result);
    let cleared = control
        .submit(&admin, command("clear2", "clear2-key", payload), 0)
        .unwrap();
    assert!(cleared.is_accepted(), "{:?}", cleared.output.result);
    assert_eq!(
        cleared.output.events[0].payload,
        EventPayload::DisplayOutputChanged {
            slot: display_slot(1),
            config: None,
        }
    );
    assert_eq!(control.snapshot().snapshot.display_outputs, Vec::new());
    let undo = control
        .submit(
            &admin,
            command(
                "u",
                "u",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undo.is_accepted(), "{:?}", undo.output.result);
    assert_wire_exact(&control, &undo);
    assert_eq!(
        undo.output.events[0].payload,
        EventPayload::DisplayOutputChanged {
            slot: display_slot(1),
            config: Some(display_config("display=index:1,source=output:8")),
        }
    );
    // The configured slot keeps the presenter's last report.
    assert_eq!(
        control.snapshot().snapshot.display_output_states[0].state,
        active
    );
}

/// IN-016/IN-017: an RTMP listener's stream key and an SRT input's
/// passphrase never leave the server (events, snapshot, and wire carry
/// `***`); `input_stream_set` keeps, replaces, or clears each secret; undo
/// restores the exact secret; and a second input listening on a taken port
/// is refused with `listen_port_in_use`.
#[test]
#[allow(clippy::too_many_lines)] // one add, edit, undo, and conflict round trip
fn rtmp_and_srt_secrets_stay_on_the_server_and_listeners_never_share_a_port() {
    use fm_protocol::SecretEdit;
    use fm_types::network_stream::{NetworkStreamConfig, StreamMode};

    let mut control = service(16, 16);
    let admin = principal(Role::Admin);
    let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
    rtmp.settings.mode = StreamMode::Listener;
    rtmp.stream_key = Some("sk_live_hunter2".into());
    let add = |id: &str, input_value: u128, config: NetworkStreamConfig| {
        command(
            id,
            &format!("{id}-key"),
            CommandPayload::AddInput {
                input: WireInputId::from_domain(input(input_value)),
                name: format!("Stream {input_value}"),
                source: LiveInputSource::NetworkStream(Box::new(config)),
            },
        )
    };
    let added = control
        .submit(&admin, add("add-rtmp", 5, rtmp.clone()), 0)
        .unwrap();
    assert!(matches!(
        added.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputAdded { network_stream: Some(config), .. },
            ..
        }] if config.stream_key.as_deref() == Some("***")
    ));
    assert_wire_exact(&control, &added);
    let leaks = |control: &ControlService, submission: &CommandSubmission, secret: &str| {
        let mut wire = String::new();
        for message in submission.output.clone().into_wire_messages() {
            wire.push_str(&fm_protocol::encode_line(&message).unwrap());
        }
        wire.push_str(
            &fm_protocol::encode_line(&WireMessage::Snapshot(control.snapshot().snapshot.clone()))
                .unwrap(),
        );
        wire.contains(secret)
    };
    assert!(!leaks(&control, &added, "sk_live_hunter2"));
    assert_eq!(control.network_stream_configs(), [(input(5), rtmp.clone())]);

    // Another listener on TCP 1935 is refused; SRT on UDP 1935 is not.
    let mut taken = NetworkStreamConfig::new("rtmp://127.0.0.1:1935/other");
    taken.settings.mode = StreamMode::Listener;
    let refused = control
        .submit(&admin, add("add-taken", 6, taken), 0)
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("listen_port_in_use"));
    assert_eq!(rejection_code(&refused), Some("conflict"));
    let mut srt = NetworkStreamConfig::new("srt://0.0.0.0:1935");
    srt.settings.mode = StreamMode::Listener;
    srt.passphrase = Some("correct-horse-battery".into());
    srt.stream_id = Some("cam1".into());
    let added = control
        .submit(&admin, add("add-srt", 7, srt.clone()), 0)
        .unwrap();
    assert!(matches!(
        added.output.result,
        CommandResult::Accepted { .. }
    ));
    assert!(!leaks(&control, &added, "correct-horse-battery"));
    let entries = &control.snapshot().snapshot.network_streams;
    assert_eq!(entries[1].config.passphrase.as_deref(), Some("***"));
    assert_eq!(entries[1].config.stream_id.as_deref(), Some("cam1"));

    // Moving the SRT input onto RTMP's port as another SRT listener is
    // fine; a second SRT listener on it is refused at edit time too.
    let set = |id: &str, input_value: u128, url: Option<&str>, stream_key, passphrase| {
        command(
            id,
            &format!("{id}-key"),
            CommandPayload::SetNetworkStream {
                input: WireInputId::from_domain(input(input_value)),
                url: url.map(fm_protocol::StreamUrl::new),
                settings: fm_types::network_stream::NetworkStreamSettings {
                    mode: StreamMode::Listener,
                    ..fm_types::network_stream::NetworkStreamSettings::default()
                },
                stream_id: None,
                stream_key,
                passphrase,
            },
        )
    };
    // Keep: the stored key stays, though the client never saw it.
    let kept = control
        .submit(
            &admin,
            set("keep", 5, None, SecretEdit::Keep, SecretEdit::Keep),
            0,
        )
        .unwrap();
    assert!(matches!(kept.output.result, CommandResult::Accepted { .. }));
    assert_eq!(
        control.network_stream_configs()[0].1.stream_key.as_deref(),
        Some("sk_live_hunter2")
    );
    // Replace, then undo back to the exact old key.
    let replaced = control
        .submit(
            &admin,
            set(
                "replace",
                5,
                None,
                SecretEdit::Set("sk_new_key".into()),
                SecretEdit::Keep,
            ),
            0,
        )
        .unwrap();
    assert!(!leaks(&control, &replaced, "sk_new_key"));
    assert_eq!(
        control.network_stream_configs()[0].1.stream_key.as_deref(),
        Some("sk_new_key")
    );
    let undone = control.submit(&admin, undo("undo-key", None), 0).unwrap();
    assert!(matches!(
        undone.output.result,
        CommandResult::Accepted { .. }
    ));
    assert!(!leaks(&control, &undone, "sk_live_hunter2"));
    assert_eq!(
        control.network_stream_configs()[0].1.stream_key.as_deref(),
        Some("sk_live_hunter2")
    );
    // Clear.
    control
        .submit(
            &admin,
            set("clear", 5, None, SecretEdit::Clear, SecretEdit::Keep),
            0,
        )
        .unwrap();
    assert_eq!(control.network_stream_configs()[0].1.stream_key, None);
    // A passphrase on RTMP is refused with its field.
    let refused = control
        .submit(
            &admin,
            set(
                "bad",
                5,
                None,
                SecretEdit::Keep,
                SecretEdit::Set("0123456789".into()),
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("invalid_passphrase"));
    // The SRT input cannot take another SRT listener's port.
    let mut second = NetworkStreamConfig::new("srt://0.0.0.0:9000");
    second.settings.mode = StreamMode::Listener;
    control
        .submit(&admin, add("add-srt2", 8, second), 0)
        .unwrap();
    let refused = control
        .submit(
            &admin,
            set(
                "collide",
                8,
                Some("srt://0.0.0.0:1935"),
                SecretEdit::Keep,
                SecretEdit::Keep,
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_field_code(&refused), Some("listen_port_in_use"));
}

/// IN-015: a network stream input added live replicates its redacted
/// configuration; `input_stream_set` edits replicate redacted; the media
/// plane's status becomes a `network_stream_status` runtime event and the
/// snapshot's entry (without the error text).
#[test]
#[allow(clippy::too_many_lines)] // one add, edit, and status round trip
fn network_stream_inputs_replicate_redacted_with_their_status() {
    use fm_types::network_stream::{
        NetworkStreamConfig, NetworkStreamState, NetworkStreamStatus, StreamUrl,
    };

    let mut control = service(16, 16);
    let admin = principal(Role::Admin);
    let added = control
        .submit(
            &admin,
            command(
                "add",
                "add-key",
                CommandPayload::AddInput {
                    input: WireInputId::from_domain(input(5)),
                    name: "Camera".to_owned(),
                    source: LiveInputSource::NetworkStream(Box::new(NetworkStreamConfig::new(
                        "rtsp://admin:hunter2@cam.test/live",
                    ))),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        added.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputAdded { network_stream: Some(config), .. },
            ..
        }] if config.url == "rtsp://***@cam.test/live"
    ));
    assert_wire_exact(&control, &added);
    // The engine keeps the full URL for the media plane.
    assert_eq!(
        control.network_stream_configs(),
        [(
            input(5),
            NetworkStreamConfig::new("rtsp://admin:hunter2@cam.test/live")
        )]
    );

    let mut settings = fm_types::network_stream::NetworkStreamSettings {
        jitter_buffer_ms: 1_000,
        ..fm_types::network_stream::NetworkStreamSettings::default()
    };
    let edited = control
        .submit(
            &admin,
            command(
                "set",
                "set-key",
                CommandPayload::SetNetworkStream {
                    input: WireInputId::from_domain(input(5)),
                    url: Some(StreamUrl::new("https://cdn.test/live.m3u8?token=abc")),
                    settings,
                    stream_id: None,
                    stream_key: fm_protocol::SecretEdit::Keep,
                    passphrase: fm_protocol::SecretEdit::Keep,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        edited.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::NetworkStreamChanged { config, .. },
            ..
        }] if config.url == "https://cdn.test/live.m3u8?token=***"
            && config.settings.jitter_buffer_ms == 1_000
    ));
    assert_wire_exact(&control, &edited);

    // Keeping the URL: only the settings change.
    settings.jitter_buffer_ms = 2_000;
    control
        .submit(
            &admin,
            command(
                "keep",
                "keep-key",
                CommandPayload::SetNetworkStream {
                    input: WireInputId::from_domain(input(5)),
                    url: None,
                    settings,
                    stream_id: None,
                    stream_key: fm_protocol::SecretEdit::Keep,
                    passphrase: fm_protocol::SecretEdit::Keep,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(
        control.network_stream_configs()[0].1.url,
        "https://cdn.test/live.m3u8?token=abc"
    );

    let entries = &control.snapshot().snapshot.network_streams;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].config.url,
        "https://cdn.test/live.m3u8?token=***"
    );
    assert_eq!(entries[0].status.state, NetworkStreamState::Idle);

    let status = NetworkStreamStatus {
        state: NetworkStreamState::Reconnecting {
            attempt: 2,
            next_in_ms: 900,
        },
        reconnects: 2,
        last_error: Some("Connection refused".into()),
        ..NetworkStreamStatus::default()
    };
    let event = control
        .set_network_stream_status(&server_identity(), input(5), status.clone())
        .unwrap()
        .expect("a changed status is published");
    assert!(matches!(
        &event.event,
        RuntimeLifecycleEvent::NetworkStreamStatus { status: published, .. }
            if **published == status
    ));
    // Unchanged, or not a network input: nothing.
    assert!(
        control
            .set_network_stream_status(&server_identity(), input(5), status.clone())
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .set_network_stream_status(&server_identity(), input(1), status.clone())
            .unwrap()
            .is_none()
    );
    let entry = &control.snapshot().snapshot.network_streams[0];
    assert_eq!(entry.status.state, status.state);
    assert_eq!(entry.status.reconnects, 2);
    assert_eq!(
        entry.status.last_error, None,
        "a snapshot carries no error text"
    );
}

/// IN-001: a capture input added live replicates its device and settings;
/// `input_capture_set` edits are undoable and replicate as
/// `capture_settings_changed`; the media plane's status becomes a
/// `capture_status` runtime event and the snapshot's entry (without the
/// error text), and its device inventory a `capture_devices` event.
#[test]
#[allow(clippy::too_many_lines)] // one add, edit, status, and inventory round trip
fn capture_inputs_replicate_with_their_status_and_inventory() {
    use fm_protocol::{
        CaptureAudio, CaptureDevice, CaptureInputConfig, CaptureInventory, CaptureLossReason,
        CaptureSettings, CaptureSignalState, CaptureStatus,
    };

    let mut control = service(16, 16);
    let admin = principal(Role::Admin);
    let config = CaptureInputConfig::new("v4l2:usb-1#C920");
    let added = control
        .submit(
            &admin,
            command(
                "add",
                "add-key",
                CommandPayload::AddInput {
                    input: WireInputId::from_domain(input(5)),
                    name: "Camera".to_owned(),
                    source: LiveInputSource::Capture(Box::new(config.clone())),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        added.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputAdded { capture: Some(added), network_stream: None, .. },
            ..
        }] if **added == config
    ));
    assert_wire_exact(&control, &added);
    assert_eq!(control.capture_inputs(), [(input(5), config.clone())]);

    let settings = CaptureSettings {
        audio: CaptureAudio::None,
        loss: fm_protocol::CaptureLossSlate::Black,
        ..CaptureSettings::default()
    };
    let edited = control
        .submit(
            &admin,
            command(
                "set",
                "set-key",
                CommandPayload::SetCaptureSettings {
                    input: WireInputId::from_domain(input(5)),
                    settings: Box::new(settings.clone()),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        edited.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::CaptureSettingsChanged { settings: changed, .. },
            ..
        }] if **changed == settings
    ));
    assert_wire_exact(&control, &edited);
    let entries = &control.snapshot().snapshot.capture_inputs;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].config.settings, settings);
    assert_eq!(entries[0].status.state, CaptureSignalState::Idle);

    // Undo restores the defaults and replicates them.
    let undone = control
        .submit(
            &admin,
            command(
                "undo",
                "undo-key",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        undone.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::CaptureSettingsChanged { settings: changed, .. },
            ..
        }] if **changed == CaptureSettings::default()
    ));

    // A non-capture input is refused.
    let refused = control
        .submit(
            &admin,
            command(
                "bad",
                "bad-key",
                CommandPayload::SetCaptureSettings {
                    input: WireInputId::from_domain(input(1)),
                    settings: Box::new(settings),
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(refused.output.events.len(), 0);

    let status = CaptureStatus {
        state: CaptureSignalState::SignalLost,
        reason: Some(CaptureLossReason::Unplugged),
        losses: 1,
        last_error: Some("No such device".into()),
        ..CaptureStatus::default()
    };
    let event = control
        .set_capture_status(&server_identity(), input(5), &status)
        .unwrap()
        .expect("a changed status is published");
    assert!(matches!(
        &event.event,
        RuntimeLifecycleEvent::CaptureStatus { status: published, .. }
            if **published == status
    ));
    assert!(fm_protocol::encode_line(&WireMessage::RuntimeEvent(event)).is_ok());
    // Unchanged, or not a capture input: nothing.
    assert!(
        control
            .set_capture_status(&server_identity(), input(5), &status)
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .set_capture_status(&server_identity(), input(1), &status)
            .unwrap()
            .is_none()
    );
    assert_eq!(control.capture_status(input(5)), Some(&status));
    let entry = &control.snapshot().snapshot.capture_inputs[0];
    assert_eq!(entry.status.state, CaptureSignalState::SignalLost);
    assert_eq!(entry.status.losses, 1);
    assert_eq!(
        entry.status.last_error, None,
        "a snapshot carries no error text"
    );

    // The inventory is bounded, published on change only.
    assert_eq!(control.capture_inventory(), &CaptureInventory::default());
    let inventory = CaptureInventory {
        backend: Some("fake".into()),
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
    let event = control
        .set_capture_inventory(&server_identity(), &inventory)
        .unwrap()
        .expect("a new inventory is published");
    assert!(matches!(
        &event.event,
        RuntimeLifecycleEvent::CaptureDevices(published) if *published == inventory
    ));
    assert!(
        control
            .set_capture_inventory(&server_identity(), &inventory)
            .unwrap()
            .is_none()
    );
    assert_eq!(control.capture_inventory(), &inventory);

    // Removing the input drops its status.
    control
        .submit(
            &admin,
            command(
                "remove",
                "remove-key",
                CommandPayload::RemoveInput {
                    input: WireInputId::from_domain(input(5)),
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(control.capture_status(input(5)), None);
    assert_eq!(control.snapshot().snapshot.capture_inputs.len(), 0);
}

/// AU-011, Protocol 2.55: a system audio input added live replicates as
/// `input_added` with its device and is listed for the media plane; its
/// status becomes a `system_audio_status` runtime event and the snapshot's
/// entry (without the error text); an undone add drops both.
#[test]
#[allow(clippy::too_many_lines)] // one add, status, and undo round trip
fn system_audio_inputs_replicate_with_their_status() {
    use fm_protocol::{SystemAudioBackend, SystemAudioConfig, SystemAudioState, SystemAudioStatus};

    let mut control = service(16, 16);
    let admin = principal(Role::Admin);
    let config = SystemAudioConfig::new(SystemAudioBackend::Pulse, "@DEFAULT_MONITOR@");
    let added = control
        .submit(
            &admin,
            command(
                "add",
                "add-key",
                CommandPayload::AddInput {
                    input: WireInputId::from_domain(input(5)),
                    name: "Desktop".to_owned(),
                    source: LiveInputSource::SystemAudio(Box::new(config.clone())),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        added.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputAdded {
                system_audio: Some(added),
                network_stream: None,
                capture: None,
                ..
            },
            ..
        }] if **added == config
    ));
    assert_wire_exact(&control, &added);
    assert_eq!(control.system_audio_inputs(), [(input(5), config.clone())]);
    let entries = &control.snapshot().snapshot.system_audio_inputs;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].config, config);
    assert_eq!(entries[0].status, SystemAudioStatus::default());

    let status = SystemAudioStatus {
        state: SystemAudioState::Reopening {
            attempt: 1,
            next_in_ms: 500,
        },
        reopens: 1,
        last_error: Some(format!("{}\nsecond line", "x".repeat(400))),
    };
    let event = control
        .set_system_audio_status(&server_identity(), input(5), &status)
        .unwrap()
        .expect("a changed status is published");
    let RuntimeLifecycleEvent::SystemAudioStatus {
        status: published, ..
    } = &event.event
    else {
        panic!("{event:?}");
    };
    let error = published.last_error.as_deref().unwrap();
    assert!(
        error.len() <= fm_protocol::MAX_SYSTEM_AUDIO_ERROR_BYTES && !error.contains('\n'),
        "{error:?}"
    );
    assert!(fm_protocol::encode_line(&WireMessage::RuntimeEvent(event.clone())).is_ok());
    // Unchanged, or not a system audio input: nothing.
    assert!(
        control
            .set_system_audio_status(&server_identity(), input(5), &status)
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .set_system_audio_status(&server_identity(), input(1), &status)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        control
            .system_audio_status(input(5))
            .map(|status| status.state),
        Some(status.state)
    );
    let entry = &control.snapshot().snapshot.system_audio_inputs[0];
    assert_eq!(entry.status.state, status.state);
    assert_eq!(entry.status.reopens, 1);
    assert_eq!(
        entry.status.last_error, None,
        "a snapshot carries no error text"
    );

    // Undoing the add removes the input, its entry, and its state.
    control
        .submit(
            &admin,
            command(
                "undo",
                "undo-key",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(control.system_audio_inputs(), []);
    assert_eq!(control.snapshot().snapshot.system_audio_inputs, []);
    assert_eq!(control.system_audio_status(input(5)), None);
}

/// IN-011: a screen capture input added live replicates as `input_added`
/// with its configuration and is listed for the media plane; its status
/// becomes a `screen_capture_status` runtime event and the snapshot's
/// entry (without the error text); an undone add drops both, and adds past
/// the bound are refused.
#[test]
#[allow(clippy::too_many_lines)] // one add, status, bound, and undo round trip
fn screen_capture_inputs_replicate_with_their_status() {
    use fm_protocol::{
        MAX_SCREEN_CAPTURE_INPUTS, ScreenCaptureAudioState, ScreenCaptureConfig,
        ScreenCaptureState, ScreenCaptureStatus, ScreenCaptureTarget, WindowRule,
    };

    let mut control = service(16, 16);
    let admin = principal(Role::Admin);
    let config = ScreenCaptureConfig::new(ScreenCaptureTarget::Window(WindowRule::exact(
        "Keynote", "Deck", None,
    )));
    let added = control
        .submit(
            &admin,
            command(
                "add",
                "add-key",
                CommandPayload::AddInput {
                    input: WireInputId::from_domain(input(5)),
                    name: "Slides".to_owned(),
                    source: LiveInputSource::ScreenCapture(Box::new(config.clone())),
                },
            ),
            0,
        )
        .unwrap();
    assert!(matches!(
        added.output.events.as_slice(),
        [EventMessage {
            payload: EventPayload::InputAdded {
                screen_capture: Some(added),
                system_audio: None,
                network_stream: None,
                capture: None,
                ..
            },
            ..
        }] if **added == config
    ));
    assert_wire_exact(&control, &added);
    assert_eq!(
        control.screen_capture_inputs(),
        [(input(5), config.clone())]
    );
    let entries = &control.snapshot().snapshot.screen_capture_inputs;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].config, config);
    assert_eq!(entries[0].status, ScreenCaptureStatus::default());

    let status = ScreenCaptureStatus {
        state: ScreenCaptureState::TargetGone,
        reopens: 1,
        width: 0,
        height: 0,
        window_id: None,
        last_error: Some(format!("{}\nsecond line", "x".repeat(400))),
        audio: ScreenCaptureAudioState::NoStream,
        audio_error: Some(format!("{}\tno stream", "y".repeat(300))),
    };
    let event = control
        .set_screen_capture_status(&server_identity(), input(5), &status)
        .unwrap()
        .expect("a changed status is published");
    let RuntimeLifecycleEvent::ScreenCaptureStatus {
        status: published, ..
    } = &event.event
    else {
        panic!("{event:?}");
    };
    let error = published.last_error.as_deref().unwrap();
    assert!(
        error.len() <= fm_protocol::MAX_SCREEN_CAPTURE_ERROR_BYTES && !error.contains('\n'),
        "{error:?}"
    );
    let audio_error = published.audio_error.as_deref().unwrap();
    assert!(
        audio_error.len() <= fm_protocol::MAX_SCREEN_CAPTURE_ERROR_BYTES
            && !audio_error.contains('\t'),
        "{audio_error:?}"
    );
    assert_eq!(published.audio, ScreenCaptureAudioState::NoStream);
    assert!(fm_protocol::encode_line(&WireMessage::RuntimeEvent(event.clone())).is_ok());
    // Unchanged, or not a screen capture input: nothing.
    assert!(
        control
            .set_screen_capture_status(&server_identity(), input(5), &status)
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .set_screen_capture_status(&server_identity(), input(1), &status)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        control
            .screen_capture_status(input(5))
            .map(|status| status.state),
        Some(ScreenCaptureState::TargetGone)
    );
    let entry = &control.snapshot().snapshot.screen_capture_inputs[0];
    assert_eq!(entry.status.state, ScreenCaptureState::TargetGone);
    assert_eq!(entry.status.reopens, 1);
    assert_eq!(
        entry.status.last_error, None,
        "a snapshot carries no error text"
    );

    // Adds past the bound are refused.
    for value in 6..u128::try_from(MAX_SCREEN_CAPTURE_INPUTS).unwrap() + 5 {
        control
            .submit(
                &admin,
                command(
                    &format!("add-{value}"),
                    &format!("add-{value}-key"),
                    CommandPayload::AddInput {
                        input: WireInputId::from_domain(input(value)),
                        name: format!("Screen {value}"),
                        source: LiveInputSource::ScreenCapture(Box::default()),
                    },
                ),
                0,
            )
            .unwrap();
    }
    assert_eq!(
        control.screen_capture_inputs().len(),
        MAX_SCREEN_CAPTURE_INPUTS
    );
    let refused = control
        .submit(
            &admin,
            command(
                "add-over",
                "add-over-key",
                CommandPayload::AddInput {
                    input: WireInputId::from_domain(input(40)),
                    name: "One too many".to_owned(),
                    source: LiveInputSource::ScreenCapture(Box::default()),
                },
            ),
            0,
        )
        .unwrap();
    assert!(refused.output.events.is_empty(), "{refused:?}");
    assert_eq!(
        control.screen_capture_inputs().len(),
        MAX_SCREEN_CAPTURE_INPUTS
    );
}

/// Protocol 2.57 (OR-017): the `LiveLAN` configuration is a padlocked,
/// undoable project edit whose PIN edit keeps, clears, or replaces the
/// stored PIN without ever echoing it; the media plane's status is a
/// runtime event and the snapshot shows it as clients should see it.
#[test]
#[allow(clippy::too_many_lines)] // one scenario from edit to runtime state
fn live_lan_is_an_undoable_project_edit_with_runtime_status() {
    use fm_protocol::{
        LiveLanEntry, LiveLanPin, LiveLanSource, LiveLanState, LiveLanStatus, SecretEdit,
    };
    let mut control = mixer_service();
    let set = |id: &str, enabled: bool, pin: SecretEdit| {
        command(
            id,
            &format!("{id}-key"),
            CommandPayload::SetLiveLan {
                config: LiveLanEntry {
                    enabled,
                    source: LiveLanSource::Output(output(8)),
                    max_viewers: 6,
                    has_pin: false,
                },
                pin,
            },
        )
    };
    assert_eq!(
        control
            .required_classes(&set("c", true, SecretEdit::Keep).payload)
            .primary(),
        CommandClass::EditProject
    );
    let denied = control
        .submit(
            &principal(Role::Operator),
            set("op", true, SecretEdit::Keep),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&denied), Some("permission_denied"));

    let admin = principal(Role::Admin);
    let applied = control
        .submit(&admin, set("one", true, SecretEdit::Set("13579".into())), 0)
        .unwrap();
    assert!(applied.is_accepted(), "{:?}", applied.output.result);
    assert_wire_exact(&control, &applied);
    let expected = LiveLanEntry {
        enabled: true,
        source: LiveLanSource::Output(output(8)),
        max_viewers: 6,
        has_pin: true,
    };
    assert_eq!(
        applied.output.events[0].payload,
        EventPayload::LiveLanChanged(expected)
    );
    assert_eq!(
        control
            .live_lan()
            .pin
            .as_ref()
            .map(LiveLanPin::expose_secret),
        Some("13579")
    );
    let snapshot = control.snapshot().snapshot.clone();
    assert_eq!(snapshot.live_lan, expected);
    // Enabled, but nothing has reported: unavailable.
    assert_eq!(snapshot.live_lan_status.state, LiveLanState::Unavailable);
    for message in applied.output.clone().into_wire_messages() {
        let line = fm_protocol::encode_line(&message).unwrap();
        assert!(!line.contains("13579"), "{line}");
    }
    let line =
        fm_protocol::encode_line(&WireMessage::Snapshot(control.snapshot().snapshot.clone()))
            .unwrap();
    assert!(!line.contains("13579"));

    // Keep keeps the PIN; an unknown output is refused.
    let kept = control
        .submit(&admin, set("two", true, SecretEdit::Keep), 0)
        .unwrap();
    assert!(kept.is_accepted(), "{:?}", kept.output.result);
    assert!(control.live_lan().pin.is_some());
    let unknown = control
        .submit(
            &admin,
            command(
                "three",
                "three-key",
                CommandPayload::SetLiveLan {
                    config: LiveLanEntry {
                        source: LiveLanSource::Output(output(99)),
                        ..expected
                    },
                    pin: SecretEdit::Keep,
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(rejection_code(&unknown), Some("not_found"));

    // The media plane reports it live, once.
    let reported = LiveLanStatus {
        state: LiveLanState::Live,
        url: "http://192.168.1.20:8090/".into(),
        viewers: 2,
        egress_kbps: 5_000,
    };
    let event = control
        .set_live_lan_status(&server_identity(), &reported)
        .unwrap()
        .expect("a changed status is published");
    assert!(matches!(
        &event.event,
        RuntimeLifecycleEvent::LiveLanStatus(status) if *status == reported
    ));
    let line = fm_protocol::encode_line(&WireMessage::RuntimeEvent(event.clone())).unwrap();
    assert_eq!(
        fm_protocol::decode_line(&line).unwrap(),
        WireMessage::RuntimeEvent(event)
    );
    assert!(
        control
            .set_live_lan_status(&server_identity(), &reported)
            .unwrap()
            .is_none()
    );
    assert_eq!(control.snapshot().snapshot.live_lan_status, reported);
    let snapshot = WireMessage::Snapshot(control.snapshot().snapshot.clone());
    let line = fm_protocol::encode_line(&snapshot).unwrap();
    assert_eq!(fm_protocol::decode_line(&line).unwrap(), snapshot);

    // Clearing the PIN and disabling: the snapshot shows it off.
    let off = control
        .submit(&admin, set("four", false, SecretEdit::Clear), 0)
        .unwrap();
    assert!(off.is_accepted(), "{:?}", off.output.result);
    assert_eq!(control.live_lan().pin, None);
    assert_eq!(
        control.snapshot().snapshot.live_lan_status,
        LiveLanStatus::default()
    );
    // Undo restores the PIN exactly.
    let undo = control
        .submit(
            &admin,
            command(
                "u",
                "u",
                CommandPayload::Undo {
                    expected_edit: None,
                },
            ),
            0,
        )
        .unwrap();
    assert!(undo.is_accepted(), "{:?}", undo.output.result);
    assert_wire_exact(&control, &undo);
    assert_eq!(
        control
            .live_lan()
            .pin
            .as_ref()
            .map(LiveLanPin::expose_secret),
        Some("13579")
    );
    // The disabled run's report is forgotten: unavailable until the media
    // plane reports again.
    assert_eq!(
        control.snapshot().snapshot.live_lan_status.state,
        LiveLanState::Unavailable
    );

    // The padlock refuses the edit.
    let lock = control
        .submit(
            &admin,
            command(
                "lock",
                "lock",
                CommandPayload::SetEditLock {
                    scope: fm_protocol::EditLockScope::Project,
                    locked: true,
                },
            ),
            0,
        )
        .unwrap();
    assert!(lock.is_accepted());
    let locked = control
        .submit(&admin, set("five", false, SecretEdit::Keep), 0)
        .unwrap();
    assert!(!locked.is_accepted());
    assert!(control.live_lan().enabled);
}

/// RC-011: script edits are graphics project edits that publish the script
/// and undo; a run is an operator transition that publishes what it did, or
/// is refused with the typed reason naming where it stopped; snapshots
/// announce the `script_state` records that follow them.
mod scripts {
    use fm_command::{RefusalReason, ScriptFailureKind};
    use fm_protocol::{ScriptSpec, WireMessage, encode_line};

    use super::*;

    fn set(script: u64, source: &str) -> CommandPayload {
        CommandPayload::SetScript {
            script,
            spec: Box::new(ScriptSpec {
                name: format!("Script {script}"),
                source: source.to_owned(),
            }),
            position: None,
        }
    }

    fn submit(
        control: &mut ControlService,
        role: Role,
        id: &str,
        payload: CommandPayload,
    ) -> CommandSubmission {
        control
            .submit(&principal(role), command(id, id, payload), 0)
            .unwrap()
    }

    fn denied(submission: &CommandSubmission) -> bool {
        matches!(
            &submission.output.result,
            CommandResult::Rejected { code, .. } if code == "permission_denied"
        )
    }

    #[test]
    fn authorization_matrix_covers_every_script_command() {
        let run = CommandPayload::RunScript { script: 1 };
        let remove = CommandPayload::RemoveScript { script: 1 };
        for (index, (role, payload, allowed)) in [
            (Role::Viewer, set(1, "cut 2"), false),
            (Role::Viewer, run.clone(), false),
            (Role::Graphics, set(1, "cut 2"), true),
            (Role::Graphics, run.clone(), false),
            (Role::Graphics, remove.clone(), true),
            (Role::Operator, set(1, "cut 2"), false),
            (Role::Operator, remove, false),
            (Role::Operator, run.clone(), true),
            (Role::Audio, run, false),
        ]
        .into_iter()
        .enumerate()
        {
            let mut control = service(8, 8);
            submit(&mut control, Role::Admin, "seed", set(1, "cut 2"));
            control.tick(&server_identity()).unwrap();
            let submission = submit(&mut control, role, &format!("c{index}"), payload.clone());
            assert_eq!(
                !denied(&submission),
                allowed,
                "{role:?} {payload:?}: {:?}",
                submission.output.result
            );
        }
    }

    #[test]
    fn edits_publish_and_undo_runs_publish_or_refuse_where_they_stopped() {
        let mut control = service(16, 16);
        assert_eq!(control.snapshot().snapshot.script_states, 0);
        let edit = submit(&mut control, Role::Admin, "set", set(1, "preview 3\ncut"));
        let EventPayload::ScriptChanged(status) = &edit.output.events[0].payload else {
            panic!("expected script_changed: {:?}", edit.output.events);
        };
        let status = (**status).clone();
        assert_eq!(
            (status.id, status.spec.source.as_str()),
            (1, "preview 3\ncut")
        );
        control.tick(&server_identity()).unwrap();

        let snapshot = control.snapshot().snapshot.clone();
        assert_eq!(snapshot.script_states, 1);
        let records = control.script_states();
        assert_eq!(records.len(), 1);
        assert_eq!(
            (records[0].revision, &records[0].script),
            (snapshot.revision, &status)
        );
        encode_line(&WireMessage::ScriptState(records[0].clone())).unwrap();

        let ran = submit(
            &mut control,
            Role::Operator,
            "run",
            CommandPayload::RunScript { script: 1 },
        );
        let EventPayload::ScriptRan(change) = &ran.output.events[0].payload else {
            panic!("expected script_ran: {:?}", ran.output.events);
        };
        assert_eq!((change.script, change.actions), (1, 2));
        assert_eq!(change.program, WireInputId::from_domain(input(3)));
        encode_line(&WireMessage::Event(ran.output.events[0].clone())).unwrap();
        control.tick(&server_identity()).unwrap();

        // A failed run names where it stopped and publishes nothing.
        submit(
            &mut control,
            Role::Admin,
            "set-bad",
            set(2, "preview 2\n  cut 9"),
        );
        control.tick(&server_identity()).unwrap();
        let refused = submit(
            &mut control,
            Role::Operator,
            "run-bad",
            CommandPayload::RunScript { script: 2 },
        );
        let CommandResult::Rejected { code, reason, .. } = &refused.output.result else {
            panic!("expected a refusal: {:?}", refused.output.result);
        };
        assert_eq!(code, "not_found");
        assert_eq!(
            reason,
            &Some(RefusalReason::ScriptFailed {
                failure: ScriptFailureKind::Refused,
                line: 2,
                column: 3,
            })
        );
        assert_eq!(refused.output.events.len(), 0);

        // Edits undo; the run did not join the history.
        let undo = submit(
            &mut control,
            Role::Admin,
            "undo",
            CommandPayload::Undo {
                expected_edit: None,
            },
        );
        assert!(
            matches!(
                undo.output.events[0].payload,
                EventPayload::ScriptRemoved { script: 2 }
            ),
            "{:?}",
            undo.output.events
        );
        control.tick(&server_identity()).unwrap();
        let remove = submit(
            &mut control,
            Role::Admin,
            "remove",
            CommandPayload::RemoveScript { script: 1 },
        );
        assert!(matches!(
            remove.output.events[0].payload,
            EventPayload::ScriptRemoved { script: 1 }
        ));
        assert_eq!(control.snapshot().snapshot.script_states, 0);
    }

    /// Protocol 2.61: a cue's `cue_run_changed` names every script step it
    /// ran with the counts a direct `script_ran` of the same script on the
    /// same show publishes.
    #[test]
    fn a_cue_run_publishes_the_scripts_it_ran() {
        let mut direct = service(16, 16);
        let mut cues = service(16, 16);
        for control in [&mut direct, &mut cues] {
            submit(control, Role::Admin, "set", set(1, "preview 3\ncut"));
            control.tick(&server_identity()).unwrap();
        }
        let ran = submit(
            &mut direct,
            Role::Operator,
            "run",
            CommandPayload::RunScript { script: 1 },
        );
        let EventPayload::ScriptRan(ran) = &ran.output.events[0].payload else {
            panic!("expected script_ran: {:?}", ran.output.events);
        };

        let list = CommandPayload::SetCueList {
            list: 4,
            spec: Box::new(fm_protocol::CueListSpec {
                name: "Show".into(),
                cues: vec![fm_protocol::WireCue {
                    id: 1,
                    name: "Open".into(),
                    auto_follow: false,
                    actions: vec![fm_protocol::WireCueAction::RunScript { script: 1 }],
                }],
            }),
            position: None,
        };
        submit(&mut cues, Role::Admin, "list", list);
        cues.tick(&server_identity()).unwrap();
        let go = submit(
            &mut cues,
            Role::Admin,
            "go",
            CommandPayload::CueControl {
                list: 4,
                operation: fm_protocol::CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default(),
                },
            },
        );
        let EventPayload::CueRunChanged(change) = &go.output.events[0].payload else {
            panic!("expected cue_run_changed: {:?}", go.output.events);
        };
        assert_eq!(
            change.scripts,
            vec![fm_protocol::CueScriptRun {
                script: 1,
                actions: ran.actions,
                instructions: ran.instructions,
            }]
        );
        assert_eq!(ran.actions, 2);
        let line = encode_line(&WireMessage::Event(go.output.events[0].clone())).unwrap();
        assert!(
            line.contains(&format!("\tscripts=1.2.{}\t", ran.instructions)),
            "{line}"
        );
    }
}

mod automation_authority;
mod list_order_journal;
mod media_lists;
mod shortcut_authority;

/// IN-014: an imported PDF becomes a paged input whose snapshot announces a
/// `paged_state` record; navigation waits for the media plane's page count,
/// then replicates as `paged_changed` with the complete status, and a source
/// report becomes a `paged` runtime event.
#[test]
#[allow(clippy::too_many_lines)] // one import, report, and navigation round trip
fn paged_inputs_step_against_the_reported_page_count() {
    use fm_protocol::{
        DocumentState, ImportItemSpec, InputLifecycleOperation, PagedKind, PagedNavigation,
    };

    let mut control = service(16, 16);
    let admin = principal(Role::Admin);
    let deck = WireInputId::from_domain(input(5));
    let imported = control
        .submit(
            &admin,
            command(
                "import",
                "import-key",
                CommandPayload::InputLifecycle(InputLifecycleOperation::Import {
                    items: vec![ImportItemSpec {
                        input: deck,
                        name: None,
                        uri: "asset://deck.pdf".to_owned(),
                    }],
                }),
            ),
            0,
        )
        .unwrap();
    assert!(
        matches!(imported.output.result, CommandResult::Accepted { .. }),
        "{:?}",
        imported.output.result
    );
    assert_wire_exact(&control, &imported);
    assert_eq!(control.snapshot().snapshot.paged_states, 1);
    assert_eq!(
        control.document_inputs(),
        [(input(5), "asset://deck.pdf".to_owned())]
    );
    let [state] = control.paged_states().try_into().unwrap();
    assert_eq!(state.status.kind, PagedKind::Document);
    assert_eq!((state.status.page, state.status.count), (0, 0));
    assert_eq!(state.status.state, DocumentState::Loading);

    let next = |control: &mut ControlService, key: &str, role| {
        control
            .submit(
                &principal(role),
                command(
                    key,
                    key,
                    CommandPayload::PageNavigate {
                        input: deck,
                        navigation: PagedNavigation::Next,
                    },
                ),
                0,
            )
            .unwrap()
    };
    // Unknown count: refused, retryably.
    let early = next(&mut control, "early", Role::Operator);
    let CommandResult::Rejected { retryable, .. } = &early.output.result else {
        panic!("{:?}", early.output.result);
    };
    assert!(*retryable);
    // A viewer may not step a document.
    assert!(matches!(
        next(&mut control, "viewer", Role::Viewer).output.result,
        CommandResult::Rejected { .. }
    ));

    let report = PagedSourceReport {
        count: Some(3),
        state: DocumentState::Ready,
        title: Some("Quarterly\nreview".to_owned()),
        message: None,
    };
    let event = control
        .set_paged_source(&server_identity(), input(5), &report)
        .unwrap()
        .expect("a changed source is published");
    let RuntimeLifecycleEvent::Paged { status } = &event.event else {
        panic!("{event:?}");
    };
    assert_eq!(status.count, 3);
    assert_eq!(status.title.as_deref(), Some("Quarterlyreview"));
    assert!(fm_protocol::encode_line(&WireMessage::RuntimeEvent(event.clone())).is_ok());
    assert!(
        control
            .set_paged_source(&server_identity(), input(5), &report)
            .unwrap()
            .is_none(),
        "an unchanged report publishes nothing"
    );
    assert!(
        control
            .set_paged_source(&server_identity(), input(1), &report)
            .unwrap()
            .is_none(),
        "only paged inputs take a report"
    );

    let stepped = next(&mut control, "next", Role::Operator);
    assert!(
        matches!(stepped.output.result, CommandResult::Accepted { .. }),
        "{:?}",
        stepped.output.result
    );
    assert_wire_exact(&control, &stepped);
    let [
        EventMessage {
            payload: EventPayload::PagedChanged { navigation, status },
            ..
        },
    ] = stepped.output.events.as_slice()
    else {
        panic!("{:?}", stepped.output.events);
    };
    assert_eq!(*navigation, PagedNavigation::Next);
    assert_eq!((status.page, status.count), (1, 3));
    assert_eq!(status.state, DocumentState::Ready);
    assert_eq!(control.paged_status(input(5)).unwrap().page, 1);
    assert_eq!(control.paged_states()[0].status.page, 1);

    // A shorter document clamps the page it reports.
    control
        .set_paged_source(
            &server_identity(),
            input(5),
            &PagedSourceReport {
                count: Some(1),
                ..report.clone()
            },
        )
        .unwrap();
    assert_eq!(control.paged_status(input(5)).unwrap().page, 0);

    // A cue's page action moves the document too; the next frame announces
    // the page clients have not heard of.
    control
        .set_paged_source(&server_identity(), input(5), &report)
        .unwrap();
    control.tick(&server_identity()).unwrap();
    let list = CommandPayload::SetCueList {
        list: 2,
        spec: Box::new(fm_protocol::CueListSpec {
            name: "Deck".into(),
            cues: vec![fm_protocol::WireCue {
                id: 1,
                name: "Last page".into(),
                auto_follow: false,
                actions: vec![fm_protocol::WireCueAction::Page {
                    input: fm_types::InputTarget::Fixed(deck),
                    navigation: PagedNavigation::Last,
                }],
            }],
        }),
        position: None,
    };
    let set = control
        .submit(&principal(Role::Admin), command("cues", "cues", list), 0)
        .unwrap();
    assert!(
        matches!(set.output.result, CommandResult::Accepted { .. }),
        "{:?}",
        set.output.result
    );
    control.tick(&server_identity()).unwrap();
    let go = control
        .submit(
            &principal(Role::Operator),
            command(
                "go",
                "go",
                CommandPayload::CueControl {
                    list: 2,
                    operation: fm_protocol::CueOperation::Go {
                        cue: None,
                        targets: fm_protocol::WireTargetBindings::default(),
                    },
                },
            ),
            0,
        )
        .unwrap();
    assert!(
        matches!(go.output.result, CommandResult::Accepted { .. }),
        "{:?}",
        go.output.result
    );
    assert_eq!(control.paged_status(input(5)).unwrap().page, 2);
    let tick = control.tick(&server_identity()).unwrap();
    let announced: Vec<_> = tick
        .runtime_events
        .iter()
        .filter_map(|event| match &event.event {
            RuntimeLifecycleEvent::Paged { status } => Some(status.page),
            _ => None,
        })
        .collect();
    assert_eq!(announced, vec![2]);
    let tick = control.tick(&server_identity()).unwrap();
    assert!(
        !tick
            .runtime_events
            .iter()
            .any(|event| matches!(event.event, RuntimeLifecycleEvent::Paged { .. })),
        "an unchanged page is announced once"
    );
}
