//! IN-024: replacing an existing alias's effect list, with undo and redo.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    Engine, EngineCommand, EngineCommandOutcome, EngineEvent, EngineInputSource, ShowState,
};
use fm_model::LayerEffect;
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "alias effects",
            [1, 2, 3]
                .into_iter()
                .map(|value| (input(value), format!("Input {value}")))
                .collect(),
            input(1),
            input(2),
        )
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(70).unwrap()),
    )
}

fn run(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap();
    engine.tick().unwrap();
    outcome
}

fn accepted(outcome: &EngineCommandOutcome) {
    assert!(
        outcome.receipt.accepted().is_some(),
        "expected acceptance: {:?}",
        outcome.receipt
    );
}

fn set_effects(value: u128, effects: Vec<LayerEffect>) -> EngineCommand {
    EngineCommand::SetAliasEffects {
        input: input(value),
        effects,
    }
}

#[test]
fn alias_effects_can_be_replaced_undone_and_redone() {
    let mut engine = engine();
    let initial = vec![LayerEffect::new("saturation", vec![-25])];
    let added = run(
        &mut engine,
        "alias",
        EngineCommand::AddInput {
            input: input(4),
            name: "Alias".into(),
            source: EngineInputSource::Alias {
                target: input(1),
                crop: None,
                effects: initial.clone(),
                opacity: u8::MAX,
                mask: None,
            },
        },
    );
    accepted(&added);
    assert!(matches!(
        added.events[0].payload,
        EngineEvent::InputAdded { input: added, .. } if added == input(4)
    ));
    assert_eq!(engine.show().alias_effects(input(4)), initial);

    let replaced = vec![LayerEffect::new("flip", vec![1, 0])];
    let changed = run(&mut engine, "effects", set_effects(4, replaced.clone()));
    accepted(&changed);
    assert!(matches!(
        changed.events[0].payload,
        EngineEvent::InputAliasEffectsChanged { input: changed, ref effects }
            if changed == input(4) && *effects == replaced
    ));
    assert_eq!(engine.show().alias_effects(input(4)), replaced);

    let undone = run(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    accepted(&undone);
    assert_eq!(engine.show().alias_effects(input(4)), initial);

    let redone = run(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    accepted(&redone);
    assert_eq!(engine.show().alias_effects(input(4)), replaced);
}

#[test]
fn a_non_alias_or_invalid_effect_list_is_refused() {
    let mut engine = engine();
    run(
        &mut engine,
        "alias",
        EngineCommand::AddInput {
            input: input(4),
            name: "Alias".into(),
            source: EngineInputSource::Alias {
                target: input(1),
                crop: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                mask: None,
            },
        },
    );

    let refused = run(
        &mut engine,
        "not-alias",
        set_effects(1, vec![LayerEffect::new("saturation", vec![0])]),
    );
    assert_eq!(
        refused.receipt.rejected().unwrap().rejection.code,
        RejectionCode::NotFound
    );

    let invalid = run(
        &mut engine,
        "invalid",
        set_effects(4, vec![LayerEffect::new("posterize", vec![4])]),
    );
    assert_eq!(
        invalid.receipt.rejected().unwrap().rejection.code,
        RejectionCode::InvalidCommand
    );
    assert!(engine.show().alias_effects(input(4)).is_empty());
}
