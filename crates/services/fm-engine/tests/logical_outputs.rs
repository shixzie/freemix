//! SW-012: logical output routing is an undoable project edit in the engine.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{Engine, EngineCommand, EngineCommandKind, ShowState};
use fm_model::{
    LogicalAudioMap, LogicalOutputConfig, LogicalOutputSlot, LogicalOutputSource,
    LogicalOverlayPolicy,
};
use fm_types::{FrameRate, InputId, OutputId};

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn output(value: u128) -> OutputId {
    OutputId::new(id(value))
}

fn slot(value: u8) -> LogicalOutputSlot {
    LogicalOutputSlot::new(value).unwrap()
}

fn engine() -> Engine {
    let show = ShowState::new(
        "logical",
        vec![(input(1), "Camera".into()), (input(2), "Slides".into())],
        input(1),
        input(2),
    )
    .unwrap()
    .with_outputs(vec![(output(40), "Program".into())])
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

fn config() -> LogicalOutputConfig {
    LogicalOutputConfig {
        slot: slot(1),
        output: output(40),
        source: LogicalOutputSource::Program,
        overlays: LogicalOverlayPolicy::All,
        audio: LogicalAudioMap::Master,
    }
}

#[test]
fn a_logical_output_is_a_project_edit_that_undoes_to_nothing() {
    assert_eq!(
        EngineCommand::SetLogicalOutput {
            slot: slot(1),
            config: None,
        }
        .kind(),
        EngineCommandKind::ProjectEdit
    );
    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetLogicalOutput {
            slot: slot(1),
            config: Some(Box::new(config())),
        },
    ));
    assert_eq!(
        engine.show().logical_output(slot(1)).unwrap().source,
        LogicalOutputSource::Program
    );

    assert!(execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().logical_output(slot(1)).is_none());

    assert!(execute(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().logical_output(slot(1)).is_some());
}

#[test]
fn an_unknown_output_is_refused_without_changing_the_show() {
    let mut engine = engine();
    let mut unknown = config();
    unknown.output = output(99);
    let outcome = engine
        .execute(
            CommandEnvelope::new(
                "command-unknown",
                IdempotencyKey::new("unknown"),
                EngineCommand::SetLogicalOutput {
                    slot: slot(1),
                    config: Some(Box::new(unknown)),
                },
            ),
            0,
        )
        .unwrap();
    assert_eq!(
        outcome
            .receipt
            .rejected()
            .expect("unknown output is refused")
            .rejection
            .code,
        RejectionCode::InvalidCommand
    );
    assert!(engine.show().logical_output(slot(1)).is_none());
}
