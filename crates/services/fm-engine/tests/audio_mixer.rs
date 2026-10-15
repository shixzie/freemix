//! AU-001 and SW-004: mixer strip labels, group membership, and group names
//! are undoable project edits behind the project and input padlocks; group
//! faders and mutes are live operations realized on the target frame; and
//! the Fade-to-Black output selection is an undoable project edit that the
//! realized frame reports per output.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineAudioGroupState, EngineAudioMixerEdit,
    EngineAudioMixerUpdate, EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent,
    EngineInputAudioStripState, ShowState,
};
use fm_model::AudioGroupNumber;
use fm_switcher::FadeToBlackFrame;
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

fn group(number: u8) -> AudioGroupNumber {
    AudioGroupNumber::new(number).unwrap()
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

fn mixer(edit: EngineAudioMixerEdit) -> EngineCommand {
    EngineCommand::AudioMixer(edit)
}

#[test]
fn command_classes_split_project_edits_from_live_group_moves() {
    let label = mixer(EngineAudioMixerEdit::SetLabel {
        input: input(1),
        label: "Host".to_owned(),
    });
    let level = mixer(EngineAudioMixerEdit::SetGroupLevel {
        group: group(1),
        state: EngineAudioGroupState {
            gain_millidb: -6_000,
            muted: false,
        },
    });
    let ftb = EngineCommand::SetFadeToBlackOutput {
        output: output(2),
        affected: false,
    };
    assert_eq!(label.kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(level.kind(), EngineCommandKind::LiveOperation);
    assert_eq!(ftb.kind(), EngineCommandKind::ProjectEdit);
}

#[test]
fn labels_groups_and_names_undo_and_redo_exactly() {
    let mut engine = engine();
    let outcome = accept(
        &mut engine,
        "label",
        mixer(EngineAudioMixerEdit::SetLabel {
            input: input(1),
            label: "Host mic".to_owned(),
        }),
    );
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::AudioMixerChanged(EngineAudioMixerEdit::SetLabel { .. })
    )));
    accept(
        &mut engine,
        "group",
        mixer(EngineAudioMixerEdit::SetGroup {
            input: input(1),
            group: Some(group(2)),
        }),
    );
    accept(
        &mut engine,
        "rename",
        mixer(EngineAudioMixerEdit::RenameGroup {
            group: group(2),
            name: "Mics".to_owned(),
        }),
    );
    assert_eq!(engine.show().input_audio_label(input(1)), "Host mic");
    assert_eq!(engine.show().input_audio_group(input(1)), Some(group(2)));
    assert_eq!(engine.show().audio_group(group(2)).name, "Mics");
    assert_eq!(engine.undo_entries().len(), 3);

    undo(&mut engine, "undo-rename");
    assert_eq!(engine.show().audio_group(group(2)).name, "Group 2");
    undo(&mut engine, "undo-group");
    assert_eq!(engine.show().input_audio_group(input(1)), None);
    undo(&mut engine, "undo-label");
    assert_eq!(engine.show().input_audio_label(input(1)), "");
    assert!(engine.show().input_audio_labels().is_empty());

    redo(&mut engine, "redo-label");
    redo(&mut engine, "redo-group");
    redo(&mut engine, "redo-rename");
    assert_eq!(engine.show().input_audio_label(input(1)), "Host mic");
    assert_eq!(engine.show().input_audio_group(input(1)), Some(group(2)));
    assert_eq!(engine.show().audio_group(group(2)).name, "Mics");
}

#[test]
fn group_levels_are_live_and_reach_the_realized_frame() {
    let mut engine = engine();
    accept(
        &mut engine,
        "group",
        mixer(EngineAudioMixerEdit::SetGroup {
            input: input(3),
            group: Some(group(1)),
        }),
    );
    let level = EngineAudioGroupState {
        gain_millidb: -12_000,
        muted: true,
    };
    accept(
        &mut engine,
        "level",
        mixer(EngineAudioMixerEdit::SetGroupLevel {
            group: group(1),
            state: level,
        }),
    );
    // A live move is never recorded.
    assert_eq!(engine.undo_entries().len(), 1);
    assert_eq!(engine.show().audio_group(group(1)).state, level);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::Membership {
                input: input(3),
                group: Some(group(1)),
            },
            EngineAudioMixerUpdate::GroupLevel {
                group: group(1),
                state: level,
            },
        ]
    );
    // Undo reverts the membership, not the live level.
    undo(&mut engine, "undo-group");
    assert_eq!(engine.show().input_audio_group(input(3)), None);
    assert_eq!(engine.show().audio_group(group(1)).state, level);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::Membership {
            input: input(3),
            group: None,
        }]
    );
}

#[test]
fn padlocks_cover_mixer_project_edits_but_not_live_moves() {
    let mut engine = engine();
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(2)),
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "locked-label",
        mixer(EngineAudioMixerEdit::SetLabel {
            input: input(2),
            label: "Guest".to_owned(),
        }),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    let outcome = execute(
        &mut engine,
        "locked-group",
        mixer(EngineAudioMixerEdit::SetGroup {
            input: input(2),
            group: Some(group(1)),
        }),
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::InputLocked)
    );
    accept(
        &mut engine,
        "free",
        mixer(EngineAudioMixerEdit::SetLabel {
            input: input(1),
            label: "Host".to_owned(),
        }),
    );
    accept(
        &mut engine,
        "project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    for (key, command) in [
        (
            "rename",
            mixer(EngineAudioMixerEdit::RenameGroup {
                group: group(1),
                name: "Band".to_owned(),
            }),
        ),
        (
            "ftb",
            EngineCommand::SetFadeToBlackOutput {
                output: output(2),
                affected: false,
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
    accept(
        &mut engine,
        "live",
        mixer(EngineAudioMixerEdit::SetGroupLevel {
            group: group(1),
            state: EngineAudioGroupState {
                gain_millidb: 0,
                muted: true,
            },
        }),
    );
}

#[test]
fn refusals_name_the_problem() {
    let mut engine = engine();
    for (key, command, code) in [
        (
            "long",
            mixer(EngineAudioMixerEdit::SetLabel {
                input: input(1),
                label: "x".repeat(fm_model::MAX_AUDIO_LABEL_BYTES + 1),
            }),
            RejectionCode::InvalidCommand,
        ),
        (
            "control",
            mixer(EngineAudioMixerEdit::SetLabel {
                input: input(1),
                label: "a\nb".to_owned(),
            }),
            RejectionCode::InvalidCommand,
        ),
        (
            "unknown",
            mixer(EngineAudioMixerEdit::SetGroup {
                input: input(9),
                group: Some(group(1)),
            }),
            RejectionCode::NotFound,
        ),
        (
            "blank",
            mixer(EngineAudioMixerEdit::RenameGroup {
                group: group(1),
                name: String::new(),
            }),
            RejectionCode::InvalidCommand,
        ),
        (
            "loud",
            mixer(EngineAudioMixerEdit::SetGroupLevel {
                group: group(1),
                state: EngineAudioGroupState {
                    gain_millidb: fm_engine::MAX_INPUT_AUDIO_GAIN_MILLIDB + 1,
                    muted: false,
                },
            }),
            RejectionCode::InvalidCommand,
        ),
        (
            "output",
            EngineCommand::SetFadeToBlackOutput {
                output: output(9),
                affected: false,
            },
            RejectionCode::NotFound,
        ),
        (
            "pan",
            EngineCommand::SetInputAudioStrip {
                input: input(1),
                state: EngineInputAudioStripState {
                    pan_basis_points: 10_001,
                    ..EngineInputAudioStripState::default()
                },
            },
            RejectionCode::InvalidCommand,
        ),
    ] {
        let outcome = execute(&mut engine, key, command);
        assert_eq!(rejection(&outcome).code, code, "{key}");
    }
    assert!(engine.show().input_audio_labels().is_empty());
    assert_eq!(engine.undo_entries().len(), 0);
}

#[test]
fn fade_to_black_output_selection_is_undoable_and_realized_per_output() {
    let mut engine = engine();
    let outcome = accept(
        &mut engine,
        "exclude",
        EngineCommand::SetFadeToBlackOutput {
            output: output(2),
            affected: false,
        },
    );
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::FadeToBlackOutputChanged {
            affected: false,
            ..
        }
    )));
    assert!(
        !engine
            .show()
            .desired_switcher()
            .fade_to_black_affects(output(2))
    );
    accept(
        &mut engine,
        "ftb",
        EngineCommand::FadeToBlack {
            active: true,
            duration_frames: 1,
        },
    );
    let frame = engine.tick().unwrap();
    assert_eq!(frame.fade_to_black_excluded_outputs, vec![output(2)]);
    assert_eq!(frame.fade_to_black_for(output(2)), FadeToBlackFrame::LIVE);
    assert_eq!(frame.fade_to_black_for(output(1)), frame.fade_to_black);
    assert_ne!(frame.fade_to_black, FadeToBlackFrame::LIVE);

    undo(&mut engine, "undo");
    assert!(
        engine
            .show()
            .desired_switcher()
            .fade_to_black_affects(output(2))
    );
    let frame = engine.tick().unwrap();
    assert_eq!(frame.fade_to_black_excluded_outputs, Vec::new());
    redo(&mut engine, "redo");
    let frame = engine.tick().unwrap();
    assert_eq!(frame.fade_to_black_excluded_outputs, vec![output(2)]);
}
