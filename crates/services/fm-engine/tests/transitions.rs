//! SW-002 and SW-005 in the engine: every automatic mix transition kind is
//! taken by kind and realized on exact frame boundaries; the four favourite
//! transition buttons are undoable project edits under the padlock; and
//! overlay channels take and leave with every motion transition.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, ShowState,
};
use fm_model::{StingerSlotNumber, TransitionEffect, TransitionFavorite, TransitionFavorites};
use fm_switcher::{
    OverlayChannelId, OverlayMotion, OverlayTransitionKind, StingerSlotId, TransitionKind,
};
use fm_types::{FrameRate, InputId, OutputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "transitions",
            vec![
                (input(1), "One".into()),
                (input(2), "Two".into()),
                (input(3), "Three".into()),
            ],
            input(1),
            input(2),
        )
        .unwrap()
        .with_outputs(vec![(
            OutputId::new(NonZeroU128::new(9).unwrap()),
            "Program".into(),
        )])
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = execute(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
    outcome
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

#[test]
fn every_mix_kind_is_taken_by_kind_on_exact_frame_boundaries() {
    for kind in TransitionKind::MIXES {
        let mut engine = engine();
        let command = EngineCommand::Transition {
            kind,
            duration_frames: 3,
        };
        assert_eq!(command.kind(), EngineCommandKind::LiveOperation);
        accept(&mut engine, "take", command);
        assert_eq!(engine.show().desired_switcher().program(), input(2));
        for (start, end) in [(0, 1), (1, 2), (2, 3)] {
            let frame = engine.tick().unwrap();
            assert_eq!(frame.program.transition_kind, Some(kind), "{kind:?}");
            assert_eq!(frame.program.primary, input(1));
            assert_eq!(frame.program.secondary, Some(input(2)));
            assert_eq!(
                (
                    frame.program.mix_start_numerator,
                    frame.program.mix_end_numerator,
                    frame.program.mix_denominator
                ),
                (start, end, 3),
                "{kind:?}"
            );
        }
        let endpoint = engine.tick().unwrap();
        assert_eq!(endpoint.program.primary, input(2), "{kind:?}");
        assert_eq!(endpoint.program.transition_kind, None);
    }
}

#[test]
fn transition_by_kind_refuses_stingers_bad_durations_and_overlap() {
    let mut engine = engine();
    let stinger = execute(
        &mut engine,
        "stinger",
        EngineCommand::Transition {
            kind: TransitionKind::Stinger(StingerSlotId::new(1).unwrap()),
            duration_frames: 10,
        },
    );
    assert_eq!(rejection(&stinger).code, RejectionCode::InvalidCommand);
    for duration_frames in [0, 3_601] {
        let refused = execute(
            &mut engine,
            &format!("duration-{duration_frames}"),
            EngineCommand::Transition {
                kind: TransitionKind::Cube,
                duration_frames,
            },
        );
        assert_eq!(rejection(&refused).code, RejectionCode::InvalidCommand);
    }
    accept(
        &mut engine,
        "cube",
        EngineCommand::Transition {
            kind: TransitionKind::Cube,
            duration_frames: 5,
        },
    );
    engine.tick().unwrap();
    let busy = execute(
        &mut engine,
        "merge",
        EngineCommand::Transition {
            kind: TransitionKind::Merge,
            duration_frames: 5,
        },
    );
    assert_eq!(rejection(&busy).code, RejectionCode::Conflict);
    assert!(rejection(&busy).message.contains("cube"));
}

fn favorite(effect: TransitionEffect, duration_frames: u32) -> TransitionFavorite {
    TransitionFavorite::new(effect, duration_frames).unwrap()
}

#[test]
fn favourites_are_undoable_project_edits_under_the_padlock() {
    let mut engine = engine();
    assert_eq!(
        engine.show().transition_favorites(),
        &TransitionFavorites::default()
    );
    let stinger = favorite(
        TransitionEffect::Stinger(StingerSlotNumber::new(2).unwrap()),
        40,
    );
    let assign = EngineCommand::SetTransitionFavorite {
        slot: 3,
        favorite: stinger,
    };
    assert_eq!(assign.kind(), EngineCommandKind::ProjectEdit);
    let outcome = accept(&mut engine, "assign", assign);
    assert!(
        outcome
            .events
            .iter()
            .any(|event| event.payload == EngineEvent::TransitionFavoriteChanged { slot: 3 })
    );
    assert_eq!(engine.show().transition_favorites().get(3), Some(stinger));
    accept(
        &mut engine,
        "assign-1",
        EngineCommand::SetTransitionFavorite {
            slot: 1,
            favorite: favorite(TransitionEffect::CrossZoom, 7),
        },
    );
    accept(
        &mut engine,
        "undo-1",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    accept(
        &mut engine,
        "undo-3",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(
        engine.show().transition_favorites(),
        &TransitionFavorites::default()
    );
    accept(
        &mut engine,
        "redo-3",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().transition_favorites().get(3), Some(stinger));

    for slot in [0, 5] {
        let refused = execute(
            &mut engine,
            &format!("slot-{slot}"),
            EngineCommand::SetTransitionFavorite {
                slot,
                favorite: stinger,
            },
        );
        assert_eq!(rejection(&refused).code, RejectionCode::InvalidCommand);
        assert_eq!(rejection(&refused).fields[0].field, "slot");
    }

    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let locked = execute(
        &mut engine,
        "locked",
        EngineCommand::SetTransitionFavorite {
            slot: 2,
            favorite: favorite(TransitionEffect::Merge, 10),
        },
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&locked)),
        Some(EditRejectionKind::ProjectLocked)
    );
}

#[test]
fn overlay_channels_take_and_leave_with_every_motion_transition() {
    let channel = OverlayChannelId::new(4).unwrap();
    for transition in OverlayTransitionKind::ALL
        .into_iter()
        .filter(|kind| kind.is_motion())
    {
        let mut engine = engine();
        accept(
            &mut engine,
            "configure",
            EngineCommand::ConfigureOverlayTransition {
                channel,
                transition,
                duration_frames: 3,
            },
        );
        engine.tick().unwrap();
        accept(
            &mut engine,
            "take",
            EngineCommand::TakeOverlay {
                channel,
                source: input(3),
            },
        );
        // The channel is fully opaque throughout; the renderer moves it in.
        for numerator in 0..3 {
            let frame = engine.tick().unwrap();
            let overlay = &frame.overlays[channel.index()];
            assert!(overlay.is_active(), "{transition:?}");
            assert_eq!(overlay.opacity(), u8::MAX);
            assert_eq!(
                overlay.motion(),
                Some(OverlayMotion {
                    kind: transition,
                    entering: true,
                    numerator,
                    denominator: 3,
                }),
                "{transition:?} frame {numerator}"
            );
        }
        let settled = engine.tick().unwrap();
        assert_eq!(settled.overlays[channel.index()].motion(), None);
        assert!(settled.overlays[channel.index()].is_active());

        accept(&mut engine, "off", EngineCommand::OverlayOff { channel });
        for numerator in 0..4 {
            let frame = engine.tick().unwrap();
            let overlay = &frame.overlays[channel.index()];
            if numerator < 3 {
                assert!(overlay.is_active(), "{transition:?}");
                assert_eq!(
                    overlay.motion(),
                    Some(OverlayMotion {
                        kind: transition,
                        entering: false,
                        numerator,
                        denominator: 3,
                    })
                );
            } else {
                assert!(
                    !overlay.is_active(),
                    "{transition:?} leaves after three frames"
                );
                assert_eq!(overlay.motion(), None);
                assert_eq!(overlay.opacity(), 0);
            }
        }
    }
}
