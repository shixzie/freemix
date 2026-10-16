//! IN-001: capture device inputs are added live with their device and
//! settings, and `SetCaptureSettings` edits are undoable project edits
//! behind the project and input padlocks; refusals name the problem and
//! change nothing. Replacing or duplicating a device keeps its settings.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineInputLifecycleEdit, EngineInputSource, ShowError,
    ShowState,
};
use fm_types::capture::{
    CaptureAudio, CaptureFormat, CaptureInputConfig, CaptureLossSlate, CapturePixelFormat,
    CaptureScan, CaptureSettings, MAX_CAPTURE_INPUTS,
};
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

fn add(value: u128, config: CaptureInputConfig) -> EngineCommand {
    EngineCommand::AddInput {
        input: input(value),
        name: format!("Camera {value}"),
        source: EngineInputSource::Capture(Box::new(config)),
    }
}

fn set(value: u128, settings: CaptureSettings) -> EngineCommand {
    EngineCommand::SetCaptureSettings {
        input: input(value),
        settings: Box::new(settings),
    }
}

fn hd() -> CaptureSettings {
    CaptureSettings {
        format: Some(CaptureFormat {
            width: 1920,
            height: 1080,
            rate: FrameRate::new(30, 1).unwrap(),
            pixel_format: CapturePixelFormat::Nv12,
            scan: CaptureScan::Progressive,
        }),
        audio: CaptureAudio::None,
        loss: CaptureLossSlate::Black,
    }
}

#[test]
fn capture_inputs_are_added_edited_and_undone_exactly() {
    let mut engine = engine();
    let config = CaptureInputConfig {
        stable_key: "v4l2:usb-1#C920".into(),
        settings: hd(),
    };
    let added = accept(&mut engine, "add", add(5, config.clone()));
    assert!(added.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::InputAdded { source: EngineInputSource::Capture(added), .. }
            if **added == config
    )));
    assert_eq!(engine.show().capture_settings(input(5)), Some(&hd()));
    assert_eq!(
        engine.show().input_source(input(5)),
        Some(&fm_model::InputSourceSpec::Device {
            stable_key: "v4l2:usb-1#C920".into()
        })
    );

    let edited = CaptureSettings {
        audio: CaptureAudio::Alsa("hw:CARD=C920,DEV=0".into()),
        ..CaptureSettings::default()
    };
    let command = set(5, edited.clone());
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    let outcome = accept(&mut engine, "set", command);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::CaptureSettingsChanged { input: changed, settings }
            if *changed == input(5) && **settings == edited
    )));
    assert_eq!(
        engine.show().capture_inputs().collect::<Vec<_>>(),
        [(input(5), "v4l2:usb-1#C920", &edited)]
    );

    accept(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().capture_settings(input(5)), Some(&hd()));
    accept(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().capture_settings(input(5)), Some(&edited));

    // Undoing the add removes the input and its settings; redoing restores
    // both exactly.
    for key in ["undo-set", "undo-add"] {
        accept(
            &mut engine,
            key,
            EngineCommand::Undo {
                expected_edit: None,
            },
        );
    }
    assert_eq!(engine.show().capture_settings(input(5)), None);
    accept(
        &mut engine,
        "redo-add",
        EngineCommand::Redo {
            expected_edit: None,
        },
    );
    assert_eq!(engine.show().capture_settings(input(5)), Some(&hd()));

    // Removing the input drops its settings.
    accept(
        &mut engine,
        "remove",
        EngineCommand::RemoveInput { input: input(5) },
    );
    assert_eq!(engine.show().capture_inputs().count(), 0);
}

#[test]
fn invalid_edits_are_refused_and_change_nothing() {
    let mut engine = engine();
    accept(&mut engine, "add", add(5, CaptureInputConfig::new("cam-5")));
    let mut bad_format = hd();
    bad_format.format.as_mut().unwrap().width = 4;
    for (key, value, settings, code, field) in [
        (
            "format",
            5,
            bad_format,
            RejectionCode::InvalidCommand,
            "format",
        ),
        (
            "audio",
            5,
            CaptureSettings {
                audio: CaptureAudio::Alsa("hw 0".into()),
                ..CaptureSettings::default()
            },
            RejectionCode::InvalidCommand,
            "audio",
        ),
        ("generator", 1, hd(), RejectionCode::InvalidCommand, "input"),
        ("unknown", 9, hd(), RejectionCode::NotFound, ""),
    ] {
        let outcome = execute(&mut engine, key, set(value, settings));
        let rejection = rejection(&outcome);
        assert_eq!(rejection.code, code, "{key}");
        if !field.is_empty() {
            assert!(
                rejection.fields.iter().any(|issue| issue.field == field),
                "{key}: {rejection:?}"
            );
        }
        assert!(outcome.events.is_empty(), "{key}");
    }
    assert_eq!(
        engine.show().capture_settings(input(5)),
        Some(&CaptureSettings::default())
    );

    // An invalid device is not added either.
    let outcome = execute(&mut engine, "bad-add", add(6, CaptureInputConfig::new(" ")));
    assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    assert!(!engine.show().inputs().contains(&input(6)));
}

#[test]
fn the_capture_input_count_is_bounded() {
    let mut engine = engine();
    let limit = u128::try_from(MAX_CAPTURE_INPUTS).unwrap();
    for value in 0..limit {
        accept(
            &mut engine,
            &format!("add-{value}"),
            add(10 + value, CaptureInputConfig::new(format!("cam-{value}"))),
        );
    }
    let outcome = execute(
        &mut engine,
        "one-too-many",
        add(100, CaptureInputConfig::new("cam-extra")),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    // Duplicating a device or replacing a generator with one is bounded
    // too.
    let outcome = execute(
        &mut engine,
        "duplicate",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::Duplicate {
            source: input(10),
            input: input(101),
            name: None,
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(engine.show().capture_inputs().count(), MAX_CAPTURE_INPUTS);
}

#[test]
fn replacing_and_duplicating_a_device_keep_its_settings() {
    let mut engine = engine();
    accept(
        &mut engine,
        "add",
        add(
            5,
            CaptureInputConfig {
                stable_key: "cam-a".into(),
                settings: hd(),
            },
        ),
    );
    accept(
        &mut engine,
        "replace",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(5),
            source: fm_model::InputSourceSpec::Device {
                stable_key: "cam-b".into(),
            },
        }),
    );
    assert_eq!(
        engine.show().capture_inputs().collect::<Vec<_>>(),
        [(input(5), "cam-b", &hd())]
    );
    accept(
        &mut engine,
        "duplicate",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::Duplicate {
            source: input(5),
            input: input(6),
            name: None,
        }),
    );
    assert_eq!(engine.show().capture_settings(input(6)), Some(&hd()));
    // Another source drops the settings; a device again starts from the
    // defaults.
    accept(
        &mut engine,
        "to-bars",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(5),
            source: fm_model::InputSourceSpec::Generator {
                video: fm_model::GeneratorVideo::Bars,
                tone_hz: None,
            },
        }),
    );
    assert_eq!(engine.show().capture_settings(input(5)), None);
    let mut show = engine.show().clone();
    assert_eq!(
        show.set_capture_settings(input(5), hd()),
        Err(ShowError::InvalidCaptureSettings(input(5)))
    );
    show.set_input_source(
        input(5),
        fm_model::InputSourceSpec::Device {
            stable_key: "cam-c".into(),
        },
    )
    .unwrap();
    assert_eq!(
        show.capture_settings(input(5)),
        Some(&CaptureSettings::default())
    );
    show.set_capture_settings(input(5), hd()).unwrap();
    assert_eq!(show.capture_settings(input(5)), Some(&hd()));
}

#[test]
fn edits_honour_the_project_and_input_padlocks() {
    let mut engine = engine();
    accept(&mut engine, "add", add(5, CaptureInputConfig::new("cam")));
    accept(
        &mut engine,
        "lock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(5)),
            locked: true,
        },
    );
    let outcome = execute(&mut engine, "input-locked", set(5, hd()));
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    accept(
        &mut engine,
        "unlock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(5)),
            locked: false,
        },
    );
    accept(
        &mut engine,
        "lock-project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let outcome = execute(&mut engine, "project-locked", set(5, hd()));
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
    assert_eq!(
        engine.show().capture_settings(input(5)),
        Some(&CaptureSettings::default())
    );
}
