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
        ..
    } = rejected.output.result
    else {
        panic!("unknown input should be rejected");
    };
    assert_eq!(code, "not_found");
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
    let before_active_transition = control.active_transition;
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
    assert_eq!(control.active_transition, before_active_transition);
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
    assert_eq!(required.additional(), Some(CommandClass::Transition));
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
    assert_eq!(control.required_classes(&redo_payload).additional(), None);

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
        Some(CommandClass::Transition)
    );
    assert_eq!(control.required_classes(&undo_payload).additional(), None);
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
            payload: EventPayload::InputAdded { input: added, name },
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
                },
                FieldDefinition {
                    id: FieldId::new(NonZeroU128::new(SCORE).unwrap()),
                    name: "Score".into(),
                    default: FieldValue::Number(0),
                },
            ],
            elements: Vec::new(),
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
                fields: vec![
                    TitleFieldStatus {
                        field: field(NAME),
                        name: "Name".into(),
                        value: TitleFieldValue::Text("Ana".into()),
                    },
                    TitleFieldStatus {
                        field: field(SCORE),
                        name: "Score".into(),
                        value: TitleFieldValue::Number(9),
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
