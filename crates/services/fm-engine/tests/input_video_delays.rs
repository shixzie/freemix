//! IN-022: per-input video delays are undoable project edits in the engine.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{Engine, EngineCommand, EngineCommandKind, ShowState};
use fm_types::{FrameRate, InputId};

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn engine() -> Engine {
    let show = ShowState::new(
        "delays",
        vec![(input(1), "Camera".into()), (input(2), "Guest".into())],
        input(1),
        input(2),
    )
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

#[test]
fn a_video_delay_is_a_project_edit_that_undoes_to_zero() {
    assert_eq!(
        EngineCommand::SetInputVideoDelay {
            input: input(1),
            frames: 5,
        }
        .kind(),
        EngineCommandKind::ProjectEdit
    );
    let mut engine = engine();
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetInputVideoDelay {
            input: input(1),
            frames: 5,
        },
    ));
    assert_eq!(engine.show().input_video_delay(input(1)), 5);
    assert_eq!(
        engine.show().input_video_delays().next(),
        Some((&input(1), &5))
    );

    assert!(execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().input_video_delay(input(1)), 0);

    assert!(execute(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().input_video_delay(input(1)), 5);

    assert!(execute(
        &mut engine,
        "clear",
        EngineCommand::SetInputVideoDelay {
            input: input(1),
            frames: 0,
        },
    ));
    assert_eq!(engine.show().input_video_delay(input(1)), 0);
    assert!(engine.show().input_video_delays().next().is_none());
}

#[test]
fn a_video_delay_aligns_the_audio_strip_delay() {
    let mut engine = engine();
    // The engine defaults to 48 kHz: five frames at 25 fps are 9,600 samples.
    assert_eq!(engine.audio_sample_rate(), 48_000);
    assert!(execute(
        &mut engine,
        "set",
        EngineCommand::SetInputVideoDelay {
            input: input(1),
            frames: 5,
        },
    ));
    assert_eq!(
        engine
            .show()
            .input_audio_strip(input(1))
            .unwrap()
            .delay_samples,
        9_600
    );

    // Undo restores both the video delay and its aligned strip delay.
    assert!(execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().input_video_delay(input(1)), 0);
    assert_eq!(
        engine
            .show()
            .input_audio_strip(input(1))
            .unwrap()
            .delay_samples,
        0
    );

    // The strip's one-second bound clamps a longer video delay.
    assert!(execute(
        &mut engine,
        "set-30",
        EngineCommand::SetInputVideoDelay {
            input: input(1),
            frames: 30,
        },
    ));
    assert_eq!(
        engine
            .show()
            .input_audio_strip(input(1))
            .unwrap()
            .delay_samples,
        48_000
    );

    // A project's own sample rate is honored.
    engine.set_audio_sample_rate(44_100);
    assert!(execute(
        &mut engine,
        "set-44100",
        EngineCommand::SetInputVideoDelay {
            input: input(2),
            frames: 5,
        },
    ));
    assert_eq!(
        engine
            .show()
            .input_audio_strip(input(2))
            .unwrap()
            .delay_samples,
        8_820
    );
}

#[test]
fn an_invalid_video_delay_is_refused_without_changing_the_show() {
    let mut engine = engine();
    assert!(!execute(
        &mut engine,
        "unknown",
        EngineCommand::SetInputVideoDelay {
            input: input(99),
            frames: 1,
        },
    ));
    assert!(!execute(
        &mut engine,
        "too-long",
        EngineCommand::SetInputVideoDelay {
            input: input(1),
            frames: fm_model::MAX_INPUT_VIDEO_DELAY_FRAMES + 1,
        },
    ));
    assert!(engine.show().input_video_delays().next().is_none());
}
