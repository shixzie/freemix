//! OR-011: fullscreen display-output slots are undoable project edits
//! behind the project padlock; refusals name the problem and change
//! nothing.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, ShowState,
};
use fm_types::display::{DisplayOutputConfig, DisplayOutputSlot};
use fm_types::{FrameRate, InputId, OutputId};

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn output(value: u128) -> OutputId {
    OutputId::new(nonzero(value))
}

fn engine() -> Engine {
    let show = ShowState::new(
        "mixer",
        (1..=3)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap()
    .with_outputs(vec![
        (output(1), "Program".to_owned()),
        (output(2), "Confidence".to_owned()),
    ])
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(nonzero(100)),
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

fn undo(engine: &mut Engine, key: &str) {
    accept(
        engine,
        key,
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
}

fn redo(engine: &mut Engine, key: &str) {
    accept(
        engine,
        key,
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
}

fn slot(value: u8) -> DisplayOutputSlot {
    DisplayOutputSlot::new(value).unwrap()
}

fn set(slot_number: u8, spec: &str) -> EngineCommand {
    EngineCommand::SetDisplayOutput {
        slot: slot(slot_number),
        config: Some(Box::new(spec.parse::<DisplayOutputConfig>().unwrap())),
    }
}

fn config(spec: &str) -> DisplayOutputConfig {
    spec.parse().unwrap()
}

#[test]
fn slots_are_project_edits_that_undo_and_redo_exactly() {
    assert_eq!(
        set(1, "display=index:0").kind(),
        EngineCommandKind::ProjectEdit
    );
    let mut engine = engine();
    let outcome = accept(
        &mut engine,
        "set",
        set(1, "display=index:0,source=output:2"),
    );
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::DisplayOutputChanged { slot: changed, config: Some(_) } if *changed == slot(1)
    )));
    accept(
        &mut engine,
        "preview",
        set(2, "display=name:Wall,source=preview,hdr=auto"),
    );
    accept(
        &mut engine,
        "retune",
        set(
            1,
            "display=index:0,source=program,mode=1280x720@50000,scaling=fill",
        ),
    );
    accept(
        &mut engine,
        "clear",
        EngineCommand::SetDisplayOutput {
            slot: slot(2),
            config: None,
        },
    );
    assert_eq!(engine.show().display_output(slot(2)), None);
    assert_eq!(engine.show().display_outputs().len(), 1);

    undo(&mut engine, "undo-clear");
    assert_eq!(
        engine.show().display_output(slot(2)),
        Some(&config("display=name:Wall,source=preview,hdr=auto"))
    );
    undo(&mut engine, "undo-retune");
    assert_eq!(
        engine.show().display_output(slot(1)),
        Some(&config("display=index:0,source=output:2"))
    );
    undo(&mut engine, "undo-preview");
    undo(&mut engine, "undo-set");
    assert!(engine.show().display_outputs().is_empty());
    for key in ["redo-set", "redo-preview", "redo-retune", "redo-clear"] {
        redo(&mut engine, key);
    }
    assert_eq!(
        engine.show().display_output(slot(1)),
        Some(&config(
            "display=index:0,source=program,mode=1280x720@50000,scaling=fill"
        ))
    );
    assert_eq!(engine.show().display_output(slot(2)), None);
}

#[test]
fn refusals_name_the_problem_and_change_nothing() {
    let mut engine = engine();
    accept(&mut engine, "set", set(1, "display=id:left"));
    for (key, command, code, text) in [
        (
            "unknown",
            set(2, "display=index:1,source=output:9"),
            RejectionCode::NotFound,
            "unknown_output",
        ),
        (
            "in-use",
            set(2, "display=id:left,source=preview"),
            RejectionCode::Conflict,
            "display_in_use",
        ),
    ] {
        let outcome = execute(&mut engine, key, command);
        assert_eq!(rejection(&outcome).code, code, "{key}");
        assert!(rejection(&outcome).message.contains(text), "{key}");
    }
    assert_eq!(engine.show().display_outputs().len(), 1);
    assert_eq!(engine.undo_entries().len(), 1);
}

#[test]
fn the_project_padlock_refuses_slot_edits() {
    let mut engine = engine();
    accept(&mut engine, "set", set(1, "display=index:0"));
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    for (key, command) in [
        ("locked-set", set(2, "display=index:1")),
        (
            "locked-clear",
            EngineCommand::SetDisplayOutput {
                slot: slot(1),
                config: None,
            },
        ),
    ] {
        let outcome = execute(&mut engine, key, command);
        assert_eq!(
            EditRejectionKind::of(rejection(&outcome)),
            Some(EditRejectionKind::ProjectLocked),
            "{key}"
        );
    }
    assert_eq!(engine.show().display_outputs().len(), 1);
}
