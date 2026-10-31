//! AU-005: strip and Master DSP chains are undoable project edits behind the
//! padlocks, realized on the target frame, and a knob drag folds into one
//! undo step.

use core::num::{NonZeroU64, NonZeroU128};

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineAudioMixerEdit, EngineAudioMixerUpdate,
    EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent, GradeGesture, ShowState,
};
use fm_types::strip_dsp::{StripDsp, StripDspPreset, StripDspTarget};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    let show = ShowState::new(
        "dsp",
        (1..=3)
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

fn set(target: StripDspTarget, chain: StripDsp, gesture: Option<GradeGesture>) -> EngineCommand {
    EngineCommand::AudioMixer(EngineAudioMixerEdit::SetStripDsp {
        target,
        chain: Box::new(chain),
        gesture,
    })
}

fn history(engine: &mut Engine, key: &str, undo: bool) {
    accept(
        engine,
        key,
        if undo {
            EngineCommand::Undo {
                expected_edit: None,
            }
        } else {
            EngineCommand::Redo {
                expected_edit: None,
            }
        },
    );
}

#[test]
fn a_chain_is_an_undoable_project_edit_realized_on_its_frame() {
    let mut engine = engine();
    let strip = StripDspTarget::Input(input(1));
    let voice = StripDspPreset::Voice.chain();
    let command = set(strip, voice, None);
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    let outcome = accept(&mut engine, "voice", command);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::AudioMixerChanged(EngineAudioMixerEdit::SetStripDsp { chain, .. })
            if **chain == voice
    )));
    assert_eq!(engine.show().input_audio_dsp(input(1)), voice);
    assert_eq!(engine.show().input_audio_dsp(input(2)), StripDsp::DEFAULT);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::StripDsp {
            target: strip,
            chain: voice,
        }]
    );

    let master = StripDspPreset::Master.chain();
    accept(
        &mut engine,
        "master",
        set(StripDspTarget::Master, master, None),
    );
    assert_eq!(engine.show().master_audio_dsp(), &master);
    assert_eq!(engine.undo_entries().len(), 2);

    history(&mut engine, "undo-master", true);
    assert_eq!(engine.show().master_audio_dsp(), &StripDsp::DEFAULT);
    history(&mut engine, "undo-voice", true);
    assert_eq!(engine.show().input_audio_dsp(input(1)), StripDsp::DEFAULT);
    assert!(engine.show().input_audio_dsps().is_empty());
    let frame = engine.tick().unwrap();
    // The Master set, then both undos, in order.
    assert_eq!(
        frame.audio_mixer_updates[1..],
        [
            EngineAudioMixerUpdate::StripDsp {
                target: StripDspTarget::Master,
                chain: StripDsp::DEFAULT,
            },
            EngineAudioMixerUpdate::StripDsp {
                target: strip,
                chain: StripDsp::DEFAULT,
            },
        ]
    );
    history(&mut engine, "redo-voice", false);
    assert_eq!(engine.show().input_audio_dsp(input(1)), voice);
}

#[test]
fn a_knob_drag_folds_into_one_undo_step() {
    let mut engine = engine();
    let strip = StripDspTarget::Input(input(2));
    let token = NonZeroU64::new(77).unwrap();
    let mut chain = StripDsp::DEFAULT;
    chain.compressor.enabled = true;
    for (step, threshold) in [-20_000, -22_000, -25_000, -30_000].into_iter().enumerate() {
        chain.compressor.threshold_millidb = threshold;
        accept(
            &mut engine,
            &format!("drag-{step}"),
            set(
                strip,
                chain,
                Some(GradeGesture {
                    token,
                    continues: step != 0,
                }),
            ),
        );
    }
    assert_eq!(engine.undo_entries().len(), 1);
    assert_eq!(engine.show().input_audio_dsp(input(2)), chain);
    history(&mut engine, "undo-drag", true);
    assert_eq!(engine.show().input_audio_dsp(input(2)), StripDsp::DEFAULT);
    history(&mut engine, "redo-drag", false);
    assert_eq!(engine.show().input_audio_dsp(input(2)), chain);

    // A drag that ends where it started leaves no step.
    let other = NonZeroU64::new(78).unwrap();
    let start = chain;
    let mut moved = chain;
    moved.compressor.ratio_milli = 8_000;
    accept(
        &mut engine,
        "round-0",
        set(
            strip,
            moved,
            Some(GradeGesture {
                token: other,
                continues: false,
            }),
        ),
    );
    accept(
        &mut engine,
        "round-1",
        set(
            strip,
            start,
            Some(GradeGesture {
                token: other,
                continues: true,
            }),
        ),
    );
    assert_eq!(engine.undo_entries().len(), 1);
    // A continuation of a different target opens its own step.
    accept(
        &mut engine,
        "master-continues",
        set(
            StripDspTarget::Master,
            StripDspPreset::Master.chain(),
            Some(GradeGesture {
                token: other,
                continues: true,
            }),
        ),
    );
    assert_eq!(engine.undo_entries().len(), 2);
}

#[test]
fn an_aux_bus_chain_is_undoable_and_folds_a_drag() {
    let mut engine = engine();
    let bus = fm_model::AuxBusNumber::from_letter('C').unwrap();
    let target = StripDspTarget::Aux(bus);
    let music = StripDspPreset::Music.chain();
    let command = set(target, music, None);
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    accept(&mut engine, "aux", command);
    assert_eq!(engine.show().aux_bus(bus).dsp, music);
    let frame = engine.tick().unwrap();
    assert_eq!(
        frame.audio_mixer_updates,
        vec![EngineAudioMixerUpdate::StripDsp {
            target,
            chain: music,
        }]
    );

    // A drag on the bus folds into one undo step like a strip's.
    let token = NonZeroU64::new(91).unwrap();
    let mut chain = music;
    chain.compressor.enabled = true;
    for (step, threshold) in [-18_000, -24_000].into_iter().enumerate() {
        chain.compressor.threshold_millidb = threshold;
        accept(
            &mut engine,
            &format!("aux-drag-{step}"),
            set(
                target,
                chain,
                Some(GradeGesture {
                    token,
                    continues: step != 0,
                }),
            ),
        );
    }
    assert_eq!(engine.undo_entries().len(), 2);
    history(&mut engine, "undo-aux-drag", true);
    assert_eq!(engine.show().aux_bus(bus).dsp, music);
    history(&mut engine, "undo-aux", true);
    assert_eq!(
        engine.show().aux_bus(bus).dsp,
        fm_types::strip_dsp::StripDsp::DEFAULT
    );
    history(&mut engine, "redo-aux", false);
    assert_eq!(engine.show().aux_bus(bus).dsp, music);

    // An aux bus has no loudness meter: the reset is accepted and changes
    // nothing.
    accept(
        &mut engine,
        "aux-loudness",
        EngineCommand::AudioMixer(EngineAudioMixerEdit::ResetLoudness { target }),
    );
}

#[test]
fn padlocks_and_bounds_refuse_chains() {
    let mut engine = engine();
    accept(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(1)),
            locked: true,
        },
    );
    let voice = StripDspPreset::Voice.chain();
    let outcome = execute(
        &mut engine,
        "locked",
        set(StripDspTarget::Input(input(1)), voice, None),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(
        EditRejectionKind::of(rejection),
        Some(EditRejectionKind::InputLocked)
    );
    accept(
        &mut engine,
        "unlocked-input",
        set(StripDspTarget::Input(input(2)), voice, None),
    );

    let mut broken = voice;
    broken.eq.bands[0].q_milli = 0;
    let outcome = execute(
        &mut engine,
        "broken",
        set(StripDspTarget::Input(input(2)), broken, None),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    let outcome = execute(
        &mut engine,
        "unknown",
        set(StripDspTarget::Input(input(9)), voice, None),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::NotFound);

    accept(
        &mut engine,
        "project-lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    );
    let outcome = execute(
        &mut engine,
        "locked-master",
        set(StripDspTarget::Master, StripDspPreset::Master.chain(), None),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(
        EditRejectionKind::of(rejection),
        Some(EditRejectionKind::ProjectLocked)
    );
    assert_eq!(engine.show().master_audio_dsp(), &StripDsp::DEFAULT);
}

#[test]
fn removing_an_input_drops_its_chain() {
    let mut engine = engine();
    let mut show = engine.show().clone();
    show.set_input_audio_dsp(input(2), StripDspPreset::Music.chain())
        .unwrap();
    assert!(
        show.set_input_audio_dsp(input(9), StripDspPreset::Music.chain())
            .is_err()
    );
    let mut broken = StripDsp::DEFAULT;
    broken.gate.release_us = 0;
    assert!(show.set_input_audio_dsp(input(2), broken).is_err());
    accept(
        &mut engine,
        "music",
        set(
            StripDspTarget::Input(input(3)),
            StripDspPreset::Music.chain(),
            None,
        ),
    );
    assert_eq!(engine.show().input_audio_dsps().len(), 1);
    accept(
        &mut engine,
        "remove",
        EngineCommand::RemoveInput { input: input(3) },
    );
    assert!(engine.show().input_audio_dsps().is_empty());
}
