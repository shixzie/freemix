//! IN-023: secondary mixes are undoable project edits in the engine.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{Engine, EngineCommand, EngineCommandKind, ShowState};
use fm_model::{MixNumber, SecondaryMixConfig, SecondaryMixSource};
use fm_types::{FrameRate, InputId};

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn mix(value: u8) -> MixNumber {
    MixNumber::new(value).unwrap()
}

fn engine() -> Engine {
    let show = ShowState::new(
        "mixes",
        vec![(input(1), "Camera".into()), (input(2), "Guest".into())],
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(id(700)),
    )
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> bool {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
        .receipt
        .accepted()
        .is_some()
}

fn config() -> SecondaryMixConfig {
    SecondaryMixConfig::output_only(mix(2), "Clean", SecondaryMixSource::Input(input(2)))
}

fn preview_config() -> SecondaryMixConfig {
    SecondaryMixConfig::preview_program(
        mix(2),
        "Clean",
        SecondaryMixSource::Input(input(1)),
        SecondaryMixSource::Input(input(2)),
    )
}

fn mix_frame(frame: &fm_engine::FrameResult) -> &fm_switcher::MixFrame {
    frame
        .secondary_mixes
        .mix(fm_switcher::MixId::new(2).unwrap())
        .expect("mix 2 is configured")
}

#[test]
fn a_cut_moves_a_live_secondary_mix() {
    use fm_switcher::MixLayer;

    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(preview_config())),
        },
    ));
    let first = engine.tick().unwrap();
    assert_eq!(mix_frame(&first).primary, MixLayer::Input(input(1)));
    assert_eq!(mix_frame(&first).preview, Some(MixLayer::Input(input(2))));
    assert_eq!(
        mix_frame(&first).primary,
        engine
            .secondary_mix_frame()
            .mix(fm_switcher::MixId::new(2).unwrap())
            .unwrap()
            .primary
    );

    engine.cut_secondary_mix(mix(2)).unwrap();
    let second = engine.tick().unwrap();
    assert_eq!(mix_frame(&second).primary, MixLayer::Input(input(2)));
    assert_eq!(mix_frame(&second).preview, Some(MixLayer::Input(input(1))));
    assert_eq!(
        engine
            .secondary_mix_frame()
            .mix(fm_switcher::MixId::new(2).unwrap())
            .unwrap()
            .primary,
        MixLayer::Input(input(2))
    );
}

#[test]
fn a_timed_secondary_mix_transition_completes() {
    use fm_switcher::{MixLayer, TransitionKind};

    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(preview_config())),
        },
    ));
    engine.tick().unwrap();
    engine
        .transition_secondary_mix(mix(2), TransitionKind::Fade, 2)
        .unwrap();

    let mut program = MixLayer::Input(input(1));
    for _ in 0..6 {
        let frame = engine.tick().unwrap();
        program = mix_frame(&frame).primary;
        if program == MixLayer::Input(input(2)) {
            break;
        }
    }
    assert_eq!(program, MixLayer::Input(input(2)));
}

#[test]
fn a_live_cut_command_moves_the_engine_bank() {
    use fm_switcher::MixLayer;

    assert_eq!(
        EngineCommand::SecondaryMixCut { number: mix(2) }.kind(),
        EngineCommandKind::LiveOperation
    );
    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(preview_config())),
        },
    ));
    engine.tick().unwrap();
    assert!(execute(
        &mut engine,
        "cut",
        EngineCommand::SecondaryMixCut { number: mix(2) },
    ));
    let frame = engine.tick().unwrap();
    assert_eq!(mix_frame(&frame).primary, MixLayer::Input(input(2)));
}

#[test]
fn a_live_transition_command_is_accepted_and_completes() {
    use fm_switcher::{MixLayer, TransitionKind};

    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(preview_config())),
        },
    ));
    engine.tick().unwrap();
    assert!(execute(
        &mut engine,
        "transition",
        EngineCommand::SecondaryMixTransition {
            number: mix(2),
            kind: TransitionKind::Fade,
            duration_frames: 2,
        },
    ));
    let mut program = MixLayer::Input(input(1));
    for _ in 0..6 {
        let frame = engine.tick().unwrap();
        program = mix_frame(&frame).primary;
        if program == MixLayer::Input(input(2)) {
            break;
        }
    }
    assert_eq!(program, MixLayer::Input(input(2)));
}

#[test]
fn a_live_cut_of_an_unknown_mix_is_refused_atomically() {
    let mut engine = engine();
    let before = engine.secondary_mix_frame().clone();
    let outcome = engine.execute(
        CommandEnvelope::new(
            "command-cut-unknown",
            IdempotencyKey::new("cut-unknown"),
            EngineCommand::SecondaryMixCut { number: mix(9) },
        ),
        0,
    );
    assert!(outcome.is_err());
    assert_eq!(engine.secondary_mix_frame(), &before);
}

#[test]
fn a_secondary_mix_is_a_project_edit_that_undoes_to_nothing() {
    assert_eq!(
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: None,
        }
        .kind(),
        EngineCommandKind::ProjectEdit
    );
    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(config())),
        },
    ));
    assert_eq!(
        engine.show().secondary_mix(mix(2)).unwrap().program,
        SecondaryMixSource::Input(input(2))
    );

    assert!(execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().secondary_mix(mix(2)).is_none());

    assert!(execute(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().secondary_mix(mix(2)).is_some());

    assert!(execute(
        &mut engine,
        "clear",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: None,
        },
    ));
    assert!(engine.show().secondary_mix(mix(2)).is_none());
}

#[test]
fn an_invalid_secondary_mix_is_refused_without_changing_the_show() {
    let mut engine = engine();
    assert!(!execute(
        &mut engine,
        "bad",
        EngineCommand::SetSecondaryMix {
            number: mix(3),
            config: Some(Box::new(SecondaryMixConfig::output_only(
                mix(3),
                "Broken",
                SecondaryMixSource::Input(input(99)),
            ))),
        },
    ));
    assert!(engine.show().secondary_mix(mix(3)).is_none());
}

#[test]
fn a_mix_referenced_by_another_cannot_be_removed_first() {
    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set-source",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(config())),
        },
    ));
    assert!(execute(
        &mut engine,
        "set-reader",
        EngineCommand::SetSecondaryMix {
            number: mix(3),
            config: Some(Box::new(SecondaryMixConfig::output_only(
                mix(3),
                "Reader",
                SecondaryMixSource::Mix(mix(2)),
            ))),
        },
    ));
    assert!(!execute(
        &mut engine,
        "remove-source",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: None,
        },
    ));
    assert!(engine.show().secondary_mix(mix(2)).is_some());
    assert!(execute(
        &mut engine,
        "remove-reader",
        EngineCommand::SetSecondaryMix {
            number: mix(3),
            config: None,
        },
    ));
    assert!(execute(
        &mut engine,
        "remove-source-now",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: None,
        },
    ));
    assert!(engine.show().secondary_mix(mix(2)).is_none());
}

#[test]
fn a_secondary_mix_fades_to_black_and_back_independently() {
    use fm_switcher::{FadeToBlackPosition, FadeToBlackTarget};

    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetSecondaryMix {
            number: mix(2),
            config: Some(Box::new(preview_config())),
        },
    ));
    engine.tick().unwrap();

    assert!(execute(
        &mut engine,
        "black",
        EngineCommand::SecondaryMixFadeToBlack {
            number: mix(2),
            active: true,
            duration_frames: 2,
        },
    ));
    let first = engine.tick().unwrap();
    assert_eq!(
        mix_frame(&first).fade_to_black.target(),
        FadeToBlackTarget::Black
    );
    engine.tick().unwrap();
    let black = engine.tick().unwrap();
    let frame = mix_frame(&black).fade_to_black;
    assert_eq!(frame.interval_start(), FadeToBlackPosition::BLACK);
    assert_eq!(frame.interval_end(), FadeToBlackPosition::BLACK);

    assert!(execute(
        &mut engine,
        "live",
        EngineCommand::SecondaryMixFadeToBlack {
            number: mix(2),
            active: false,
            duration_frames: 2,
        },
    ));
    let first = engine.tick().unwrap();
    assert_eq!(
        mix_frame(&first).fade_to_black.target(),
        FadeToBlackTarget::Live
    );
    engine.tick().unwrap();
    let live = engine.tick().unwrap();
    assert_eq!(
        mix_frame(&live).fade_to_black,
        fm_switcher::FadeToBlackFrame::LIVE
    );
}
