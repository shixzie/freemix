//! SW-016: per-input freeze is a live, non-undoable engine operation.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{Engine, EngineCommand, EngineCommandKind, ShowState};
use fm_types::{FrameRate, InputId};

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn engine() -> Engine {
    let show = ShowState::new(
        "freeze",
        vec![(input(1), "Camera".into()), (input(2), "Guest".into())],
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(id(701)),
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

#[test]
fn freeze_is_a_live_operation_and_is_not_undone() {
    assert_eq!(
        EngineCommand::SetInputFreeze {
            input: input(1),
            frozen: true,
        }
        .kind(),
        EngineCommandKind::LiveOperation
    );
    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "freeze",
        EngineCommand::SetInputFreeze {
            input: input(1),
            frozen: true,
        },
    ));
    assert!(engine.show().is_input_frozen(input(1)));
    assert!(!engine.show().is_input_frozen(input(2)));

    // Undo has nothing to revert: freeze is runtime state, not an edit.
    assert!(execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().is_input_frozen(input(1)));

    assert!(execute(
        &mut engine,
        "unfreeze",
        EngineCommand::SetInputFreeze {
            input: input(1),
            frozen: false,
        },
    ));
    assert!(!engine.show().is_input_frozen(input(1)));
    assert_eq!(engine.show().frozen_inputs().count(), 0);
}

#[test]
fn an_unknown_input_cannot_be_frozen() {
    let mut engine = engine();
    assert!(!execute(
        &mut engine,
        "missing",
        EngineCommand::SetInputFreeze {
            input: input(99),
            frozen: true,
        },
    ));
    assert_eq!(engine.show().frozen_inputs().count(), 0);
}
