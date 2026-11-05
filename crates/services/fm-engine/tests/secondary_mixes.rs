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
