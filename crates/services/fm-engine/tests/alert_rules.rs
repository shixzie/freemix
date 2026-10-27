//! RC-015 / Protocol 2.112: a show's alert rule settings are undoable
//! project edits; a setting without a hysteresis band is refused, and the
//! rule's default removes the show's own setting.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{Engine, EngineCommand, EngineCommandOutcome, EngineEvent, ShowState};
use fm_types::alert_rules::{AlertRuleSetting, HealthAlertRule};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "alerts",
        (1..=2)
            .map(|value| (input(value), format!("Camera {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(200).unwrap()),
    )
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap();
    while engine.work_in_flight() {
        engine.tick().unwrap();
    }
    outcome
}

fn changed(outcome: &EngineCommandOutcome) -> Option<Vec<AlertRuleSetting>> {
    outcome
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EngineEvent::AlertRulesChanged(settings) => Some(settings.clone()),
            _ => None,
        })
}

#[test]
fn alert_rule_settings_are_undoable_project_edits() {
    let mut engine = engine();
    assert!(engine.show().alert_rules().is_empty());
    let off = AlertRuleSetting {
        enabled: false,
        ..AlertRuleSetting::default_for(HealthAlertRule::OutputFailed)
    };
    let outcome = execute(&mut engine, "off", EngineCommand::SetAlertRule(off));
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(changed(&outcome), Some(vec![off]));
    assert_eq!(engine.show().alert_rules(), [off]);

    // No band: refused, nothing changes.
    let outcome = execute(
        &mut engine,
        "no-band",
        EngineCommand::SetAlertRule(AlertRuleSetting {
            clear_milli: 2_000,
            ..off
        }),
    );
    let rejection = &outcome.receipt.rejected().expect("refused").rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    assert_eq!(rejection.fields[0].code, "invalid_alert_rule");
    assert_eq!(engine.show().alert_rules(), [off]);

    // Undo restores the default; redo switches the rule off again.
    let outcome = execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(changed(&outcome), Some(Vec::new()));
    assert!(engine.show().alert_rules().is_empty());
    execute(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().alert_rules(), [off]);

    // Setting the default removes the show's own setting.
    let outcome = execute(
        &mut engine,
        "default",
        EngineCommand::SetAlertRule(AlertRuleSetting::default_for(HealthAlertRule::OutputFailed)),
    );
    assert_eq!(changed(&outcome), Some(Vec::new()));
    assert!(engine.show().alert_rules().is_empty());
}
