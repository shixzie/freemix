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
    EngineAudioMixerUpdate, EngineAudioMonitor, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineInputAudioStripState, ShowState,
};
use fm_model::{AudioGroupNumber, AudioSoloMode};
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

#[test]
#[allow(clippy::too_many_lines)]
fn fade_to_black_output_durations_and_curve_are_padlocked_undoable_and_realized() {
    let mut engine = engine();
    let duration = |frames| EngineCommand::SetFadeToBlackOutputDuration {
        output: output(2),
        duration_frames: frames,
    };
    let ease = EngineCommand::SetFadeToBlackCurve {
        curve: fm_switcher::FadeToBlackCurve::Ease,
    };
    assert_eq!(duration(Some(2)).kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(ease.kind(), EngineCommandKind::ProjectEdit);
    for (key, command, code) in [
        ("zero", duration(Some(0)), RejectionCode::InvalidCommand),
        ("long", duration(Some(3_601)), RejectionCode::InvalidCommand),
        (
            "unknown",
            EngineCommand::SetFadeToBlackOutputDuration {
                output: output(9),
                duration_frames: Some(2),
            },
            RejectionCode::NotFound,
        ),
    ] {
        let outcome = execute(&mut engine, key, command);
        assert_eq!(rejection(&outcome).code, code, "{key}");
    }
    let outcome = accept(&mut engine, "own", duration(Some(2)));
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::FadeToBlackOutputDurationChanged {
            duration_frames: Some(2),
            ..
        }
    )));
    accept(&mut engine, "ease", ease.clone());
    let switcher = engine.show().desired_switcher();
    assert_eq!(switcher.fade_to_black_output_duration(output(2)), Some(2));
    assert_eq!(
        switcher.fade_to_black_curve(),
        fm_switcher::FadeToBlackCurve::Ease
    );

    // The padlock covers both edits.
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    for (key, command) in [("locked-own", duration(None)), ("locked-ease", ease)] {
        let outcome = execute(&mut engine, key, command);
        assert_eq!(
            EditRejectionKind::of(rejection(&outcome)),
            Some(EditRejectionKind::ProjectLocked),
            "{key}"
        );
    }
    accept(
        &mut engine,
        "unlock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: false,
        },
    );

    // Output 2 fades over its own two frames on the eased curve, Program
    // over the four the request carries; each frame reports output 2's
    // interval wherever it differs.
    accept(
        &mut engine,
        "ftb",
        EngineCommand::FadeToBlack {
            active: true,
            duration_frames: 4,
        },
    );
    let mut own = Vec::new();
    let mut shared = Vec::new();
    for _ in 0..4 {
        let frame = engine.tick().unwrap();
        own.push(
            frame
                .fade_to_black_for(output(2))
                .interval_end()
                .numerator(),
        );
        shared.push(
            frame
                .fade_to_black_for(output(1))
                .interval_end()
                .numerator(),
        );
        assert_eq!(frame.fade_to_black_for(output(1)), frame.fade_to_black);
        assert_eq!(
            frame
                .fade_to_black_overrides
                .iter()
                .any(|(candidate, _)| *candidate == output(2)),
            frame.fade_to_black_for(output(2)) != frame.fade_to_black
        );
    }
    assert_eq!(own, [32_767, 65_535, 65_535, 65_535]);
    assert_eq!(shared, [10_239, 32_767, 55_295, 65_535]);

    // Undo restores the shared duration and the linear curve, in order.
    undo(&mut engine, "undo-ease");
    assert_eq!(
        engine.show().desired_switcher().fade_to_black_curve(),
        fm_switcher::FadeToBlackCurve::Linear
    );
    undo(&mut engine, "undo-own");
    assert_eq!(
        engine
            .show()
            .desired_switcher()
            .fade_to_black_output_duration(output(2)),
        None
    );
    redo(&mut engine, "redo-own");
    assert_eq!(
        engine
            .show()
            .desired_switcher()
            .fade_to_black_output_duration(output(2)),
        Some(2)
    );
}

#[test]
fn solo_mode_and_group_parents_are_undoable_project_edits_and_group_solo_is_live() {
    let mut engine = engine();
    let solo = mixer(EngineAudioMixerEdit::SetGroupSolo {
        group: group(1),
        soloed: true,
    });
    let parent = mixer(EngineAudioMixerEdit::SetGroupParent {
        group: group(1),
        parent: Some(group(2)),
    });
    let mode = mixer(EngineAudioMixerEdit::SetSoloMode {
        mode: AudioSoloMode::Pfl,
    });
    assert_eq!(solo.kind(), EngineCommandKind::LiveOperation);
    assert_eq!(parent.kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(mode.kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(engine.show().audio_solo_mode(), AudioSoloMode::InPlace);

    accept(&mut engine, "mode", mode);
    accept(&mut engine, "parent", parent);
    let outcome = accept(&mut engine, "solo", solo);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::AudioMixerChanged(EngineAudioMixerEdit::SetGroupSolo { soloed: true, .. })
    )));
    assert_eq!(engine.show().audio_solo_mode(), AudioSoloMode::Pfl);
    assert_eq!(engine.show().audio_group(group(1)).parent, Some(group(2)));
    assert!(engine.show().audio_group(group(1)).soloed);
    // Only the two project edits are recorded.
    assert_eq!(engine.undo_entries().len(), 2);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::SoloMode(AudioSoloMode::Pfl),
            EngineAudioMixerUpdate::GroupParent {
                group: group(1),
                parent: Some(group(2)),
            },
            EngineAudioMixerUpdate::GroupSolo {
                group: group(1),
                soloed: true,
            },
        ]
    );

    undo(&mut engine, "undo-parent");
    assert_eq!(engine.show().audio_group(group(1)).parent, None);
    // Undo never reverts the live solo.
    assert!(engine.show().audio_group(group(1)).soloed);
    undo(&mut engine, "undo-mode");
    assert_eq!(engine.show().audio_solo_mode(), AudioSoloMode::InPlace);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::GroupParent {
                group: group(1),
                parent: None,
            },
            EngineAudioMixerUpdate::SoloMode(AudioSoloMode::InPlace),
        ]
    );
    redo(&mut engine, "redo-mode");
    redo(&mut engine, "redo-parent");
    assert_eq!(engine.show().audio_solo_mode(), AudioSoloMode::Pfl);
    assert_eq!(engine.show().audio_group(group(1)).parent, Some(group(2)));
}

#[test]
fn group_parents_refuse_cycles_and_the_padlock_covers_parent_and_mode() {
    let mut engine = engine();
    for (child, parent) in [(1, 2), (2, 3), (3, 4)] {
        accept(
            &mut engine,
            &format!("parent-{child}"),
            mixer(EngineAudioMixerEdit::SetGroupParent {
                group: group(child),
                parent: Some(group(parent)),
            }),
        );
    }
    for (key, child, parent) in [("self", 2, 2), ("loop", 4, 1), ("short", 3, 1)] {
        let outcome = execute(
            &mut engine,
            key,
            mixer(EngineAudioMixerEdit::SetGroupParent {
                group: group(child),
                parent: Some(group(parent)),
            }),
        );
        let rejection = rejection(&outcome);
        assert_eq!(rejection.code, RejectionCode::InvalidCommand, "{key}");
        assert!(rejection.message.contains("nested inside itself"), "{key}");
    }
    assert_eq!(engine.show().audio_group(group(4)).parent, None);
    let mut show = engine.show().clone();
    assert_eq!(
        show.set_audio_group(
            group(4),
            fm_engine::EngineAudioGroup {
                parent: Some(group(1)),
                ..engine.show().audio_group(group(4)).clone()
            },
        ),
        Err(fm_engine::ShowError::AudioGroupCycle(group(4)))
    );

    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    for (key, command) in [
        (
            "locked-parent",
            mixer(EngineAudioMixerEdit::SetGroupParent {
                group: group(4),
                parent: None,
            }),
        ),
        (
            "locked-mode",
            mixer(EngineAudioMixerEdit::SetSoloMode {
                mode: AudioSoloMode::Afl,
            }),
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
        "live-solo",
        mixer(EngineAudioMixerEdit::SetGroupSolo {
            group: group(4),
            soloed: true,
        }),
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn the_monitor_device_is_an_undoable_padlocked_edit_and_its_level_is_live() {
    let mut engine = engine();
    let device = |device: Option<&str>| {
        mixer(EngineAudioMixerEdit::SetMonitorDevice {
            device: device.map(str::to_owned),
        })
    };
    let level = |gain_millidb, muted| {
        mixer(EngineAudioMixerEdit::SetMonitorLevel {
            state: EngineAudioGroupState {
                gain_millidb,
                muted,
            },
        })
    };
    assert_eq!(device(None).kind(), EngineCommandKind::ProjectEdit);
    assert_eq!(level(0, false).kind(), EngineCommandKind::LiveOperation);
    assert_eq!(
        engine.show().audio_monitor(),
        &EngineAudioMonitor::default()
    );

    accept(&mut engine, "device", device(Some("hw:CARD=Headset,DEV=0")));
    accept(&mut engine, "level", level(-6_000, true));
    assert_eq!(
        engine.show().audio_monitor(),
        &EngineAudioMonitor {
            device: Some("hw:CARD=Headset,DEV=0".into()),
            state: EngineAudioGroupState {
                gain_millidb: -6_000,
                muted: true,
            },
        }
    );
    // Only the device is recorded.
    assert_eq!(engine.undo_entries().len(), 1);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::MonitorDevice(Some("hw:CARD=Headset,DEV=0".into())),
            EngineAudioMixerUpdate::MonitorLevel(EngineAudioGroupState {
                gain_millidb: -6_000,
                muted: true,
            }),
        ]
    );

    // Undo restores the exact previous device and never the live level.
    undo(&mut engine, "undo-device");
    assert_eq!(engine.show().audio_monitor().device, None);
    assert!(engine.show().audio_monitor().state.muted);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::MonitorDevice(None)]
    );
    redo(&mut engine, "redo-device");
    assert_eq!(
        engine.show().audio_monitor().device.as_deref(),
        Some("hw:CARD=Headset,DEV=0")
    );
    engine.tick().unwrap();

    // Choosing the current device again retries it on the media plane but
    // records nothing: one undo still restores the device before it.
    let recorded = engine.undo_entries().len();
    accept(&mut engine, "retry", device(Some("hw:CARD=Headset,DEV=0")));
    assert_eq!(
        engine.undo_entries().len(),
        recorded,
        "a retry is not an edit"
    );
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::MonitorDevice(Some(
            "hw:CARD=Headset,DEV=0".into()
        ))]
    );
    undo(&mut engine, "undo-after-retry");
    assert_eq!(engine.show().audio_monitor().device, None);
    // A retry keeps the redo history too.
    accept(&mut engine, "retry-none", device(None));
    redo(&mut engine, "redo-after-retry");
    assert_eq!(
        engine.show().audio_monitor().device.as_deref(),
        Some("hw:CARD=Headset,DEV=0")
    );

    // An unusable device or level is refused and changes nothing.
    for (key, command, field) in [
        ("spaces", device(Some("my headset")), "device"),
        ("empty", device(Some("")), "device"),
        ("option", device(Some("-report")), "device"),
        ("loud", level(24_001, false), "gain_millidb"),
        ("quiet", level(-96_001, false), "gain_millidb"),
    ] {
        let outcome = execute(&mut engine, key, command);
        let rejection = rejection(&outcome);
        assert_eq!(rejection.code, RejectionCode::InvalidCommand, "{key}");
        assert!(
            rejection.fields.iter().any(|issue| issue.field == field),
            "{key}: {rejection:?}"
        );
    }
    assert_eq!(engine.show().audio_monitor().state.gain_millidb, -6_000);
    let mut show = engine.show().clone();
    assert_eq!(
        show.set_audio_monitor(EngineAudioMonitor {
            device: Some("two words".into()),
            ..EngineAudioMonitor::default()
        }),
        Err(fm_engine::ShowError::InvalidAudioMonitorDevice)
    );

    // The padlock covers the device but not the level.
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let outcome = execute(&mut engine, "locked-device", device(None));
    assert_eq!(
        EditRejectionKind::of(rejection(&outcome)),
        Some(EditRejectionKind::ProjectLocked)
    );
    accept(&mut engine, "locked-level", level(0, false));
}

/// AU-005: the loudness reset is a live operation that changes no show
/// state: never recorded, never blocked by a padlock, refused only for an
/// unknown input, and realized on its frame for the media plane.
#[test]
fn loudness_resets_are_live_unrecorded_and_realized_on_their_frame() {
    use fm_types::strip_dsp::StripDspTarget;
    let mut engine = engine();
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let show = engine.show().clone();
    let undo_entries = engine.undo_entries().len();
    for (key, target) in [
        ("master", StripDspTarget::Master),
        ("strip", StripDspTarget::Input(input(2))),
    ] {
        let edit = EngineAudioMixerEdit::ResetLoudness { target };
        assert!(edit.is_live());
        assert_eq!(edit.locked_input(), None);
        accept(&mut engine, key, mixer(edit));
    }
    assert_eq!(engine.show(), &show);
    assert_eq!(engine.undo_entries().len(), undo_entries);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::LoudnessReset {
                target: StripDspTarget::Master,
            },
            EngineAudioMixerUpdate::LoudnessReset {
                target: StripDspTarget::Input(input(2)),
            },
        ]
    );
    let revision = engine.revision();
    let outcome = execute(
        &mut engine,
        "unknown",
        mixer(EngineAudioMixerEdit::ResetLoudness {
            target: StripDspTarget::Input(input(9)),
        }),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    assert_eq!(engine.revision(), revision);
}

fn aux(letter: char) -> fm_model::AuxBusNumber {
    fm_model::AuxBusNumber::from_letter(letter).unwrap()
}

#[test]
fn aux_buses_rename_and_route_outputs_undoably_and_move_their_faders_live() {
    let mut engine = engine();
    assert_eq!(engine.show().aux_bus(aux('A')).name, "Bus A");
    accept(
        &mut engine,
        "rename",
        mixer(EngineAudioMixerEdit::RenameAuxBus {
            bus: aux('B'),
            name: "Stage".to_owned(),
        }),
    );
    accept(
        &mut engine,
        "route",
        mixer(EngineAudioMixerEdit::SetOutputAuxBus {
            output: output(2),
            bus: Some(aux('B')),
        }),
    );
    let level = EngineAudioGroupState {
        gain_millidb: -6_000,
        muted: true,
    };
    let live = mixer(EngineAudioMixerEdit::SetAuxBusLevel {
        bus: aux('B'),
        state: level,
    });
    assert_eq!(live.kind(), EngineCommandKind::LiveOperation);
    accept(&mut engine, "level", live);
    assert_eq!(engine.show().aux_bus(aux('B')).name, "Stage");
    assert_eq!(engine.show().aux_bus(aux('B')).state, level);
    assert_eq!(engine.show().output_aux_bus(output(2)), Some(aux('B')));
    assert_eq!(engine.show().output_aux_bus(output(1)), None);
    // The rename and the route are recorded; the live fader move is not.
    assert_eq!(engine.undo_entries().len(), 2);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![
            EngineAudioMixerUpdate::OutputAuxBus {
                output: output(2),
                bus: Some(aux('B')),
            },
            EngineAudioMixerUpdate::AuxBusLevel {
                bus: aux('B'),
                state: level,
            },
        ]
    );

    undo(&mut engine, "undo-route");
    assert_eq!(engine.show().output_aux_bus(output(2)), None);
    undo(&mut engine, "undo-rename");
    assert_eq!(engine.show().aux_bus(aux('B')).name, "Bus B");
    // Undo never reverts the live fader.
    assert_eq!(engine.show().aux_bus(aux('B')).state, level);
    redo(&mut engine, "redo-rename");
    redo(&mut engine, "redo-route");
    assert_eq!(engine.show().aux_bus(aux('B')).name, "Stage");
    assert_eq!(engine.show().output_aux_bus(output(2)), Some(aux('B')));
}

#[test]
fn aux_bus_refusals_name_the_problem_and_strip_buses_are_live() {
    let mut engine = engine();
    let unknown = execute(
        &mut engine,
        "unknown-output",
        mixer(EngineAudioMixerEdit::SetOutputAuxBus {
            output: output(9),
            bus: Some(aux('A')),
        }),
    );
    assert_eq!(rejection(&unknown).code, RejectionCode::NotFound);
    let blank = execute(
        &mut engine,
        "blank",
        mixer(EngineAudioMixerEdit::RenameAuxBus {
            bus: aux('A'),
            name: "  ".to_owned(),
        }),
    );
    assert_eq!(rejection(&blank).code, RejectionCode::InvalidCommand);
    let loud = execute(
        &mut engine,
        "loud",
        mixer(EngineAudioMixerEdit::SetAuxBusLevel {
            bus: aux('A'),
            state: EngineAudioGroupState {
                gain_millidb: 24_001,
                muted: false,
            },
        }),
    );
    assert_eq!(rejection(&loud).code, RejectionCode::InvalidCommand);
    assert_eq!(
        engine.show().aux_bus(aux('A')),
        &fm_engine::EngineAuxBus::default_for(aux('A'))
    );

    // A strip's sends ride its live strip state: off Master, onto C and G,
    // with G at -6 dB pre-fader.
    let sends = fm_model::AudioSends::from_mask(fm_model::AudioBusMask::from_token("CG").unwrap())
        .with_aux_send(
            aux('G'),
            fm_model::BusSendState {
                enabled: true,
                level_millidb: -6_000,
                tap: fm_model::SendTap::PreFader,
            },
        );
    let state = EngineInputAudioStripState {
        sends,
        ..EngineInputAudioStripState::default()
    };
    let strip = EngineCommand::SetInputAudioStrip {
        input: input(2),
        state,
    };
    assert_eq!(strip.kind(), EngineCommandKind::LiveOperation);
    accept(&mut engine, "buses", strip);
    assert_eq!(engine.show().input_audio_strip(input(2)), Some(state));
    let frame = engine.tick().unwrap();
    assert!(frame.input_audio_strip_updates.contains(&(input(2), state)));
}

#[test]
fn output_mix_minus_names_a_known_input_and_undoes_exactly() {
    let mut engine = engine();
    assert_eq!(engine.show().output_mix_minus_input(output(2)), None);
    accept(
        &mut engine,
        "mix-minus",
        mixer(EngineAudioMixerEdit::SetOutputMixMinus {
            output: output(2),
            input: Some(input(3)),
        }),
    );
    assert_eq!(
        engine.show().output_mix_minus_input(output(2)),
        Some(input(3))
    );
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::OutputMixMinus {
            output: output(2),
            input: Some(input(3)),
        }]
    );
    undo(&mut engine, "undo-mix-minus");
    assert_eq!(engine.show().output_mix_minus_input(output(2)), None);
    redo(&mut engine, "redo-mix-minus");
    assert_eq!(
        engine.show().output_mix_minus_input(output(2)),
        Some(input(3))
    );
    // Unknown outputs and inputs are refused without changing the show.
    for (key, edit) in [
        (
            "unknown-output",
            EngineAudioMixerEdit::SetOutputMixMinus {
                output: output(9),
                input: Some(input(3)),
            },
        ),
        (
            "unknown-input",
            EngineAudioMixerEdit::SetOutputMixMinus {
                output: output(2),
                input: Some(input(9)),
            },
        ),
    ] {
        let outcome = execute(&mut engine, key, mixer(edit));
        assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);
    }
    assert_eq!(
        engine.show().output_mix_minus_input(output(2)),
        Some(input(3))
    );
    // Clearing the feed is its own undoable edit.
    accept(
        &mut engine,
        "clear-mix-minus",
        mixer(EngineAudioMixerEdit::SetOutputMixMinus {
            output: output(2),
            input: None,
        }),
    );
    assert_eq!(engine.show().output_mix_minus_input(output(2)), None);
    undo(&mut engine, "undo-clear-mix-minus");
    assert_eq!(
        engine.show().output_mix_minus_input(output(2)),
        Some(input(3))
    );
}

#[test]
fn aux_bus_devices_are_project_edits_that_undo_and_refuse_unusable_names() {
    let mut engine = engine();
    assert_eq!(engine.show().aux_bus(aux('B')).device, None);
    accept(
        &mut engine,
        "aux-device",
        mixer(EngineAudioMixerEdit::SetAuxBusDevice {
            bus: aux('B'),
            device: Some("plughw:CARD=Headset,DEV=0".into()),
        }),
    );
    assert_eq!(
        engine.show().aux_bus(aux('B')).device.as_deref(),
        Some("plughw:CARD=Headset,DEV=0")
    );
    let frame = engine.tick().unwrap();
    assert!(
        frame
            .audio_mixer_updates
            .contains(&EngineAudioMixerUpdate::AuxBusDevice {
                bus: aux('B'),
                device: Some("plughw:CARD=Headset,DEV=0".into()),
            })
    );
    undo(&mut engine, "undo-aux-device");
    assert_eq!(engine.show().aux_bus(aux('B')).device, None);
    redo(&mut engine, "redo-aux-device");
    assert_eq!(
        engine.show().aux_bus(aux('B')).device.as_deref(),
        Some("plughw:CARD=Headset,DEV=0")
    );
    // An unusable device is refused without changing the show.
    for bad in ["two words", "-report", ""] {
        let outcome = execute(
            &mut engine,
            "bad-aux-device",
            mixer(EngineAudioMixerEdit::SetAuxBusDevice {
                bus: aux('B'),
                device: Some(bad.into()),
            }),
        );
        assert_eq!(rejection(&outcome).code, RejectionCode::InvalidCommand);
    }
    assert_eq!(
        engine.show().aux_bus(aux('B')).device.as_deref(),
        Some("plughw:CARD=Headset,DEV=0")
    );
    // Clearing it is its own undoable edit.
    accept(
        &mut engine,
        "clear-aux-device",
        mixer(EngineAudioMixerEdit::SetAuxBusDevice {
            bus: aux('B'),
            device: None,
        }),
    );
    assert_eq!(engine.show().aux_bus(aux('B')).device, None);
}
