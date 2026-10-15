//! IN-026 in the engine: category list, category, colour label, pause,
//! close, duplicate, source replacement, batch import, and multi-input
//! removal as journaled project edits with undo, redo, and padlocks; pause
//! holds a media transport and close releases its source.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandOutcome, EngineEvent,
    EngineInputAudioStripState, EngineInputLifecycleEdit, EngineMediaSource, ShowState,
};
use fm_model::{
    BatchImportRefusal, BatchImportRequest, GeneratorVideo, InputCategory, InputCategoryId,
    InputColorLabel, InputLifecycle, InputSourceSpec, MediaPlaybackSettings,
};
use fm_types::{FrameRate, InputId};

const CLIP: u128 = 4;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn category(value: u32) -> InputCategoryId {
    InputCategoryId::new(value).unwrap()
}

fn rate() -> FrameRate {
    FrameRate::new(25, 1).unwrap()
}

fn clip_source() -> InputSourceSpec {
    InputSourceSpec::Media {
        asset_uri: "asset://clip.mov".into(),
    }
}

/// Inputs 1 (Program), 2 (Preview), 3, and media input 4 playing from a
/// synthetic source.
fn engine() -> Engine {
    let mut show = ShowState::new(
        "lifecycle",
        [(1, "Camera"), (2, "Slides"), (3, "Guest"), (CLIP, "Clip")]
            .into_iter()
            .map(|(value, name)| (input(value), name.to_owned()))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    for value in 1..=3 {
        show.set_input_source(
            input(value),
            InputSourceSpec::Generator {
                video: GeneratorVideo::Bars,
                tone_hz: None,
            },
        )
        .unwrap();
    }
    let settings = MediaPlaybackSettings::default();
    show.set_media_input(input(CLIP), settings).unwrap();
    show.set_input_source(input(CLIP), clip_source()).unwrap();
    let mut engine = Engine::new(
        show,
        rate(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    engine
        .set_media_source(
            input(CLIP),
            Some(EngineMediaSource::synthetic(&settings, rate())),
        )
        .unwrap();
    engine
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

fn edit(engine: &mut Engine, key: &str, edit: EngineInputLifecycleEdit) -> EngineCommandOutcome {
    run(engine, key, EngineCommand::InputLifecycle(edit))
}

fn accept(engine: &mut Engine, key: &str, change: EngineInputLifecycleEdit) -> EngineEvent {
    let outcome = edit(engine, key, change);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
    assert_eq!(outcome.events.len(), 1, "{key}");
    outcome.events[0].payload.clone()
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

fn issue(outcome: &EngineCommandOutcome) -> &str {
    &rejection(outcome).fields[0].code
}

const UNDO: EngineCommand = EngineCommand::Undo {
    expected_edit: None,
};
const REDO: EngineCommand = EngineCommand::Redo {
    expected_edit: None,
};

fn categories() -> Vec<InputCategory> {
    vec![
        InputCategory::new(category(1), "Cameras"),
        InputCategory::new(category(2), "Clips"),
    ]
}

#[test]
#[allow(clippy::too_many_lines)] // one scenario across every label edit
fn labels_pause_and_close_are_undoable_project_edits() {
    let mut engine = engine();
    accept(
        &mut engine,
        "categories",
        EngineInputLifecycleEdit::SetCategories {
            categories: categories(),
        },
    );
    accept(
        &mut engine,
        "category",
        EngineInputLifecycleEdit::SetCategory {
            input: input(3),
            category: Some(category(2)),
        },
    );
    accept(
        &mut engine,
        "colour",
        EngineInputLifecycleEdit::SetColor {
            input: input(3),
            color: Some(InputColorLabel::Red),
        },
    );
    accept(
        &mut engine,
        "pause",
        EngineInputLifecycleEdit::SetPaused {
            input: input(3),
            paused: true,
        },
    );
    accept(
        &mut engine,
        "close",
        EngineInputLifecycleEdit::SetClosed {
            input: input(3),
            closed: true,
        },
    );
    let labelled = InputLifecycle {
        category: Some(category(2)),
        color: Some(InputColorLabel::Red),
        paused: true,
        closed: true,
    };
    assert_eq!(engine.show().input_lifecycle(input(3)), labelled);
    assert_eq!(engine.show().input_categories(), categories().as_slice());

    // Undo walks back one edit at a time; redo replays them.
    for key in ["undo-close", "undo-pause"] {
        assert!(run(&mut engine, key, UNDO).receipt.accepted().is_some());
    }
    assert_eq!(
        engine.show().input_lifecycle(input(3)),
        InputLifecycle {
            paused: false,
            closed: false,
            ..labelled
        }
    );
    assert!(run(&mut engine, "redo", REDO).receipt.accepted().is_some());
    assert!(engine.show().input_lifecycle(input(3)).paused);

    // Unknown inputs and categories, a category still in use, and closing
    // Program are refused.
    let unknown_input = edit(
        &mut engine,
        "unknown-input",
        EngineInputLifecycleEdit::SetColor {
            input: input(9),
            color: None,
        },
    );
    assert_eq!(issue(&unknown_input), "unknown_input");
    let unknown_category = edit(
        &mut engine,
        "unknown-category",
        EngineInputLifecycleEdit::SetCategory {
            input: input(3),
            category: Some(category(7)),
        },
    );
    assert_eq!(issue(&unknown_category), "unknown_category");
    let in_use = edit(
        &mut engine,
        "in-use",
        EngineInputLifecycleEdit::SetCategories {
            categories: categories()[..1].to_vec(),
        },
    );
    assert_eq!(issue(&in_use), "category_in_use");
    let on_air = edit(
        &mut engine,
        "on-air",
        EngineInputLifecycleEdit::SetClosed {
            input: input(1),
            closed: true,
        },
    );
    assert_eq!(issue(&on_air), "input_in_use");

    // A padlocked input refuses its lifecycle edits.
    assert!(
        run(
            &mut engine,
            "lock",
            EngineCommand::SetEditLock {
                scope: EditLockScope::Input(input(3)),
                locked: true,
            },
        )
        .receipt
        .accepted()
        .is_some()
    );
    let locked = edit(
        &mut engine,
        "locked",
        EngineInputLifecycleEdit::SetPaused {
            input: input(3),
            paused: false,
        },
    );
    assert_eq!(
        EditRejectionKind::of(rejection(&locked)),
        Some(EditRejectionKind::InputLocked)
    );
}

#[test]
fn a_duplicate_copies_labels_and_strip_and_undo_removes_it() {
    let mut engine = engine();
    accept(
        &mut engine,
        "categories",
        EngineInputLifecycleEdit::SetCategories {
            categories: categories(),
        },
    );
    accept(
        &mut engine,
        "category",
        EngineInputLifecycleEdit::SetCategory {
            input: input(3),
            category: Some(category(1)),
        },
    );
    accept(
        &mut engine,
        "pause",
        EngineInputLifecycleEdit::SetPaused {
            input: input(3),
            paused: true,
        },
    );
    let strip = EngineInputAudioStripState {
        gain_millidb: -6_000,
        ..engine.show().input_audio_strip(input(3)).unwrap()
    };
    assert!(
        run(
            &mut engine,
            "strip",
            EngineCommand::SetInputAudioStrip {
                input: input(3),
                state: strip,
            },
        )
        .receipt
        .accepted()
        .is_some()
    );
    let event = accept(
        &mut engine,
        "duplicate",
        EngineInputLifecycleEdit::Duplicate {
            source: input(3),
            input: input(10),
            name: None,
        },
    );
    let EngineEvent::InputLifecycleChanged { added, refused, .. } = event else {
        panic!("expected input_lifecycle_changed, got {event:?}");
    };
    assert_eq!(added, [(input(10), "Guest 2".to_owned())]);
    assert_eq!(refused, Vec::new());
    assert_eq!(engine.show().inputs().last(), Some(&input(10)));
    assert_eq!(engine.show().input_audio_strip(input(10)), Some(strip));
    assert_eq!(
        engine.show().input_lifecycle(input(10)),
        InputLifecycle {
            category: Some(category(1)),
            ..InputLifecycle::default()
        }
    );
    assert_eq!(
        engine.show().input_source(input(10)),
        engine.show().input_source(input(3))
    );
    let taken = edit(
        &mut engine,
        "taken",
        EngineInputLifecycleEdit::Duplicate {
            source: input(3),
            input: input(10),
            name: None,
        },
    );
    assert_eq!(issue(&taken), "duplicate_input");

    assert!(run(&mut engine, "undo", UNDO).receipt.accepted().is_some());
    assert!(!engine.show().inputs().contains(&input(10)));
    assert!(run(&mut engine, "redo", REDO).receipt.accepted().is_some());
    assert_eq!(engine.show().inputs().last(), Some(&input(10)));
    assert_eq!(engine.show().input_name(input(10)), Some("Guest 2"));
}

#[test]
fn a_batch_import_adds_accepted_items_and_reports_refusals() {
    let mut engine = engine();
    let request = |id, uri: &str| BatchImportRequest {
        input: input(id),
        name: None,
        asset_uri: uri.to_owned(),
    };
    let event = accept(
        &mut engine,
        "import",
        EngineInputLifecycleEdit::Import {
            items: vec![
                request(20, "asset://Interview.mov"),
                request(21, "/host/never-imported.mov"),
                request(22, "asset://b-roll.mp4"),
                request(3, "asset://other.mkv"),
            ],
        },
    );
    let EngineEvent::InputLifecycleChanged { added, refused, .. } = event else {
        panic!("expected input_lifecycle_changed, got {event:?}");
    };
    assert_eq!(
        added,
        [
            (input(20), "Interview".to_owned()),
            (input(22), "b-roll".to_owned()),
        ]
    );
    assert_eq!(
        refused,
        [
            (1, BatchImportRefusal::Unavailable),
            (3, BatchImportRefusal::DuplicateInput),
        ]
    );
    for value in [20, 22] {
        assert!(engine.show().media_settings(input(value)).is_some());
        assert!(
            engine
                .show()
                .input_source(input(value))
                .is_some_and(InputSourceSpec::is_media)
        );
    }
    // A batch with nothing to add is refused as a whole.
    let empty = edit(
        &mut engine,
        "nothing",
        EngineInputLifecycleEdit::Import {
            items: vec![request(30, "notes.txt")],
        },
    );
    assert!(empty.receipt.rejected().is_some());

    // Undo removes exactly the imported inputs; redo brings them back.
    assert!(run(&mut engine, "undo", UNDO).receipt.accepted().is_some());
    assert_eq!(engine.show().inputs().len(), 4);
    assert!(run(&mut engine, "redo", REDO).receipt.accepted().is_some());
    assert_eq!(engine.show().inputs().len(), 6);

    // Removing several inputs at once forgets the history naming them.
    accept(
        &mut engine,
        "remove",
        EngineInputLifecycleEdit::RemoveInputs {
            inputs: vec![input(20), input(22)],
        },
    );
    assert_eq!(engine.show().inputs().len(), 4);
    assert!(engine.undo_entries().all(|entry| {
        !matches!(
            entry.forward(),
            EngineCommand::InputLifecycle(EngineInputLifecycleEdit::Import { .. })
        )
    }));
}

#[test]
fn replacing_a_source_is_undoable_and_moves_media_settings() {
    let mut engine = engine();
    accept(
        &mut engine,
        "replace",
        EngineInputLifecycleEdit::ReplaceSource {
            input: input(3),
            source: clip_source(),
        },
    );
    assert_eq!(engine.show().input_source(input(3)), Some(&clip_source()));
    assert!(engine.show().media_settings(input(3)).is_some());
    assert!(run(&mut engine, "undo", UNDO).receipt.accepted().is_some());
    assert!(matches!(
        engine.show().input_source(input(3)),
        Some(InputSourceSpec::Generator { .. })
    ));
    assert!(engine.show().media_settings(input(3)).is_none());
    let unusable = edit(
        &mut engine,
        "unusable",
        EngineInputLifecycleEdit::ReplaceSource {
            input: input(3),
            source: InputSourceSpec::Media {
                asset_uri: "asset://notes.txt".into(),
            },
        },
    );
    assert!(unusable.receipt.rejected().is_some());
}

#[test]
fn pause_holds_a_media_transport_and_close_releases_its_source() {
    let mut engine = engine();
    assert!(
        run(
            &mut engine,
            "play",
            EngineCommand::Media(fm_engine::EngineMediaCommand {
                input: input(CLIP),
                operation: fm_engine::EngineMediaOperation::Play,
            }),
        )
        .receipt
        .accepted()
        .is_some()
    );
    for _ in 0..5 {
        engine.tick().unwrap();
    }
    accept(
        &mut engine,
        "pause",
        EngineInputLifecycleEdit::SetPaused {
            input: input(CLIP),
            paused: true,
        },
    );
    let held = engine.media_status(input(CLIP)).unwrap().position;
    assert!(held > 0);
    for _ in 0..10 {
        engine.tick().unwrap();
    }
    let status = engine.media_status(input(CLIP)).unwrap();
    assert_eq!(status.position, held, "a paused input holds its frame");
    assert!(status.available);
    accept(
        &mut engine,
        "resume",
        EngineInputLifecycleEdit::SetPaused {
            input: input(CLIP),
            paused: false,
        },
    );
    engine.tick().unwrap();
    assert!(engine.media_status(input(CLIP)).unwrap().position > held);

    // Closing releases the source with one tickless media frame; the media
    // plane opens it again once the input reopens.
    assert!(engine.media_source_open(input(CLIP)));
    engine
        .execute(
            CommandEnvelope::new(
                "command-close",
                IdempotencyKey::new("close"),
                EngineCommand::InputLifecycle(EngineInputLifecycleEdit::SetClosed {
                    input: input(CLIP),
                    closed: true,
                }),
            ),
            0,
        )
        .unwrap();
    assert!(!engine.media_source_open(input(CLIP)));
    let frame = engine.tick().unwrap();
    let released = frame
        .media
        .iter()
        .find(|media| media.input == input(CLIP))
        .expect("the release is reported");
    assert!(released.tick.is_none());
    assert!(!engine.media_status(input(CLIP)).unwrap().available);
    accept(
        &mut engine,
        "reopen",
        EngineInputLifecycleEdit::SetClosed {
            input: input(CLIP),
            closed: false,
        },
    );
    assert!(!engine.media_source_open(input(CLIP)));
    let settings = MediaPlaybackSettings::default();
    engine
        .set_media_source(
            input(CLIP),
            Some(EngineMediaSource::synthetic(&settings, rate())),
        )
        .unwrap();
    assert!(engine.media_source_open(input(CLIP)));
    assert!(engine.media_status(input(CLIP)).unwrap().available);
}
