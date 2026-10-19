//! AU-011: system audio inputs are added live with their audio system and
//! device, replace other sources and are replaced, and are undone like any
//! other add; an unusable device and a ninth input are refused and change
//! nothing.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    Engine, EngineCommand, EngineCommandOutcome, EngineInputLifecycleEdit, EngineInputSource,
    ShowState,
};
use fm_model::InputSourceSpec;
use fm_types::system_audio::{MAX_SYSTEM_AUDIO_INPUTS, SystemAudioBackend, SystemAudioConfig};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "mixer",
        (1..=2)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
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

fn monitor() -> SystemAudioConfig {
    SystemAudioConfig::new(SystemAudioBackend::Pulse, "@DEFAULT_MONITOR@")
}

fn add(value: u128, config: SystemAudioConfig) -> EngineCommand {
    EngineCommand::AddInput {
        input: input(value),
        name: format!("System Audio {value}"),
        source: EngineInputSource::SystemAudio(Box::new(config)),
    }
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

#[test]
fn system_audio_inputs_are_added_replaced_and_undone_exactly() {
    let mut engine = engine();
    accept(&mut engine, "add", add(5, monitor()));
    assert_eq!(
        engine.show().system_audio_inputs().collect::<Vec<_>>(),
        [(input(5), &monitor())]
    );
    undo(&mut engine, "undo-add");
    assert_eq!(engine.show().system_audio_inputs().count(), 0);
    assert!(!engine.show().inputs().contains(&input(5)));
    accept(
        &mut engine,
        "redo-add",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().system_audio_inputs().count(), 1);

    // Replacing the device keeps the input; another source drops it.
    let loopback = SystemAudioConfig::new(SystemAudioBackend::Alsa, "hw:Loopback,1");
    accept(
        &mut engine,
        "replace",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(5),
            source: InputSourceSpec::SystemAudio(loopback.clone()),
        }),
    );
    assert_eq!(
        engine.show().system_audio_inputs().collect::<Vec<_>>(),
        [(input(5), &loopback)]
    );
    accept(
        &mut engine,
        "to-bars",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(5),
            source: InputSourceSpec::Generator {
                video: fm_model::GeneratorVideo::Bars,
                tone_hz: None,
            },
        }),
    );
    assert_eq!(engine.show().system_audio_inputs().count(), 0);
    undo(&mut engine, "undo-bars");
    assert_eq!(
        engine.show().system_audio_inputs().collect::<Vec<_>>(),
        [(input(5), &loopback)]
    );
    // A generator becomes a system audio input too.
    accept(&mut engine, "add-bars", add_bars(6));
    accept(
        &mut engine,
        "generator",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(6),
            source: InputSourceSpec::SystemAudio(monitor()),
        }),
    );
    assert_eq!(engine.show().system_audio_inputs().count(), 2);
}

fn add_bars(value: u128) -> EngineCommand {
    EngineCommand::AddInput {
        input: input(value),
        name: format!("Bars {value}"),
        source: EngineInputSource::Bars,
    }
}

#[test]
fn an_unusable_device_is_refused_and_changes_nothing() {
    let mut engine = engine();
    for (key, device) in [
        ("empty", String::new()),
        ("option", "-f lavfi".to_owned()),
        ("long", "x".repeat(129)),
        ("control", "hw:0\n".to_owned()),
    ] {
        let outcome = execute(
            &mut engine,
            key,
            add(5, SystemAudioConfig::new(SystemAudioBackend::Alsa, device)),
        );
        let rejection = rejection(&outcome);
        assert_eq!(rejection.code, RejectionCode::InvalidCommand, "{key}");
        assert!(
            rejection.fields.iter().any(|issue| issue.field == "device"),
            "{key}: {rejection:?}"
        );
        assert!(outcome.events.is_empty(), "{key}");
        assert!(!engine.show().inputs().contains(&input(5)), "{key}");
    }
    accept(&mut engine, "add-bars", add_bars(6));
    let outcome = execute(
        &mut engine,
        "replace-bad",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(6),
            source: InputSourceSpec::SystemAudio(SystemAudioConfig::new(
                SystemAudioBackend::Pulse,
                " ",
            )),
        }),
    );
    assert!(outcome.receipt.rejected().is_some());
    assert_eq!(engine.show().system_audio_inputs().count(), 0);
}

#[test]
fn the_system_audio_input_count_is_bounded() {
    let mut engine = engine();
    let limit = u128::try_from(MAX_SYSTEM_AUDIO_INPUTS).unwrap();
    for value in 0..limit {
        accept(
            &mut engine,
            &format!("add-{value}"),
            add(10 + value, monitor()),
        );
    }
    let outcome = execute(&mut engine, "one-more", add(10 + limit, monitor()));
    let rejection = rejection(&outcome);
    assert_eq!(rejection.code, RejectionCode::Conflict);
    assert!(
        rejection
            .fields
            .iter()
            .any(|issue| issue.code == "too_many_system_audio_inputs"),
        "{rejection:?}"
    );
    // A replacement onto a ninth input is refused the same way; another
    // device on a system audio input is not a ninth.
    accept(&mut engine, "add-bars", add_bars(6));
    let outcome = execute(
        &mut engine,
        "replace-one-more",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(6),
            source: InputSourceSpec::SystemAudio(monitor()),
        }),
    );
    assert_eq!(rejection_code(&outcome), Some(RejectionCode::Conflict));
    accept(
        &mut engine,
        "replace-device",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(10),
            source: InputSourceSpec::SystemAudio(SystemAudioConfig::new(
                SystemAudioBackend::Alsa,
                "hw:Loopback,1",
            )),
        }),
    );
    assert_eq!(
        engine.show().system_audio_inputs().count(),
        MAX_SYSTEM_AUDIO_INPUTS
    );
}

fn rejection_code(outcome: &EngineCommandOutcome) -> Option<RejectionCode> {
    outcome
        .receipt
        .rejected()
        .map(|rejected| rejected.rejection.code)
}
