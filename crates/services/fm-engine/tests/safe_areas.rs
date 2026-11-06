//! SW-014: the operator safe-area guide selection is a padlocked, undoable
//! project edit in the engine.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{Engine, EngineCommand, EngineEvent, ShowState};
use fm_model::SafeAreaGuideSelection;
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "safe areas",
            vec![(input(1), "Camera".into()), (input(2), "Guest".into())],
            input(1),
            input(2),
        )
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(700).unwrap()),
    )
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> Vec<EngineEvent> {
    let outcome = engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap();
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    outcome
        .events
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

#[test]
fn safe_area_guides_are_a_project_edit_with_undo_and_redo() {
    let mut engine = engine();
    assert_eq!(
        engine.show().safe_area_guides(),
        SafeAreaGuideSelection::default()
    );
    let all = SafeAreaGuideSelection {
        action: true,
        title: true,
        center_4_3: true,
        center_16_9: true,
        center_9_16: true,
    };
    let events = execute(
        &mut engine,
        "set",
        EngineCommand::SetSafeAreaGuides { guides: all },
    );
    assert!(events.contains(&EngineEvent::SafeAreaGuidesChanged { guides: all }));
    assert_eq!(engine.show().safe_area_guides(), all);

    execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(
        engine.show().safe_area_guides(),
        SafeAreaGuideSelection::default()
    );
    execute(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().safe_area_guides(), all);
}

#[test]
fn no_guides_is_distinct_from_the_title_safe_default() {
    let mut engine = engine();
    execute(
        &mut engine,
        "none",
        EngineCommand::SetSafeAreaGuides {
            guides: SafeAreaGuideSelection::NONE,
        },
    );
    assert_eq!(
        engine.show().safe_area_guides(),
        SafeAreaGuideSelection::NONE
    );
    execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(
        engine.show().safe_area_guides(),
        SafeAreaGuideSelection::default()
    );
}
