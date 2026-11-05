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
        vec![
            (input(1), "Camera".into()),
            (input(2), "Slides".into()),
            (input(3), "Guest".into()),
        ],
        input(1),
        input(2),
    )
    .unwrap()
    .with_outputs(vec![
        (output(40), "Program".into()),
        (output(41), "Preview".into()),
        (output(42), "Guest".into()),
        (output(43), "Grid".into()),
    ])
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
fn a_frame_resolves_program_preview_input_and_multiview_outputs() {
    use fm_switcher::{AudioSource, CellFeed, LogicalOutputSlot as WireSlot, OutputVideo};

    let mut engine = engine();
    for (key, slot_number, source, audio) in [
        (1, 1, LogicalOutputSource::Program, LogicalAudioMap::Master),
        (2, 2, LogicalOutputSource::Preview, LogicalAudioMap::Master),
        (
            3,
            3,
            LogicalOutputSource::Input(input(3)),
            LogicalAudioMap::FollowVideo,
        ),
        (
            4,
            4,
            LogicalOutputSource::MultiView(vec![
                fm_model::LogicalMultiViewCell::Program,
                fm_model::LogicalMultiViewCell::Input(input(3)),
                fm_model::LogicalMultiViewCell::Output(slot(1)),
            ]),
            LogicalAudioMap::Master,
        ),
    ] {
        assert!(execute(
            &mut engine,
            &format!("set-{key}"),
            EngineCommand::SetLogicalOutput {
                slot: slot(slot_number),
                config: Some(Box::new(LogicalOutputConfig {
                    slot: slot(slot_number),
                    output: output(39 + u128::from(slot_number)),
                    source,
                    overlays: LogicalOverlayPolicy::All,
                    audio,
                })),
            },
        ));
    }
    let frame = engine.tick().unwrap();
    let plan = |number| {
        frame
            .logical_outputs
            .output(WireSlot::new(number).unwrap())
            .expect("enabled")
            .as_ref()
            .expect("resolved")
    };
    assert_eq!(plan(1).audio, AudioSource::Master);
    assert!(matches!(plan(1).video, OutputVideo::Background(_)));
    assert_eq!(plan(2).audio, AudioSource::Master);
    assert!(matches!(plan(2).video, OutputVideo::Background(_)));
    assert_eq!(plan(3).audio, AudioSource::Input(input(3)));
    let OutputVideo::MultiView(cells) = &plan(4).video else {
        panic!("a multiview plan");
    };
    assert_eq!(cells.len(), 3);
    assert!(matches!(cells[0].feed, CellFeed::Background(_)));
    assert!(matches!(cells[1].feed, CellFeed::Background(_)));
    assert!(matches!(cells[2].feed, CellFeed::Output(_)));
}

#[test]
fn a_frame_resolves_configured_secondary_mixes_and_follow_video_audio() {
    use fm_model::{MixNumber, SecondaryMixConfig, SecondaryMixSource};
    use fm_switcher::{AudioSource, FeedLayer, OutputVideo};

    let mut engine = engine();
    let mix = MixNumber::new(2).unwrap();
    assert!(execute(
        &mut engine,
        "set-mix",
        EngineCommand::SetSecondaryMix {
            number: mix,
            config: Some(Box::new(SecondaryMixConfig::output_only(
                mix,
                "Clean",
                SecondaryMixSource::Input(input(3)),
            ))),
        },
    ));
    assert!(execute(
        &mut engine,
        "set-mix-output",
        EngineCommand::SetLogicalOutput {
            slot: slot(1),
            config: Some(Box::new(LogicalOutputConfig {
                slot: slot(1),
                output: output(40),
                source: LogicalOutputSource::Mix(2),
                overlays: LogicalOverlayPolicy::All,
                audio: LogicalAudioMap::FollowVideo,
            })),
        },
    ));
    let frame = engine.tick().unwrap();
    let plan = frame
        .logical_outputs
        .output(fm_switcher::LogicalOutputSlot::new(1).unwrap())
        .expect("enabled")
        .as_ref()
        .expect("resolved");
    let OutputVideo::Background(background) = plan.video else {
        panic!("a background plan");
    };
    assert_eq!(background.primary, FeedLayer::Input(input(3)));
    assert_eq!(plan.audio, AudioSource::Input(input(3)));
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
