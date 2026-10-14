//! Live input add and remove: catalogue rules, padlocks, and undo behaviour.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineInputAudioStripState, EngineInputSource,
    MAX_SHOW_INPUTS, ShowState,
};
use fm_switcher::{
    MissingMediaFallback, OverlayChannelId, StingerAudioPolicy, StingerDescriptor, StingerSlotId,
};
use fm_types::{FrameRate, InputId};

const RED: EngineInputSource = EngineInputSource::Solid {
    red: 255,
    green: 0,
    blue: 0,
    alpha: 255,
};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "catalogue show",
            [1, 2, 3]
                .into_iter()
                .map(|value| (input(value), format!("Input {value}")))
                .collect(),
            input(1),
            input(2),
        )
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn envelope(key: &str, command: EngineCommand) -> CommandEnvelope<EngineCommand> {
    CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command)
}

/// Executes one command and realizes it with a single tick.
fn run(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = engine.execute(envelope(key, command), 0).unwrap();
    engine.tick().unwrap();
    outcome
}

fn add(value: u128, name: &str, source: EngineInputSource) -> EngineCommand {
    EngineCommand::AddInput {
        input: input(value),
        name: name.to_owned(),
        source,
    }
}

fn remove(value: u128) -> EngineCommand {
    EngineCommand::RemoveInput {
        input: input(value),
    }
}

const UNDO: EngineCommand = EngineCommand::Undo {
    expected_edit: None,
};
const REDO: EngineCommand = EngineCommand::Redo {
    expected_edit: None,
};

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

fn assert_accepted(outcome: &EngineCommandOutcome) {
    assert!(
        outcome.receipt.accepted().is_some(),
        "expected acceptance, got {:?}",
        outcome.receipt
    );
}

fn ids(engine: &Engine) -> Vec<u128> {
    engine
        .show()
        .inputs()
        .iter()
        .map(|input| input.get().get())
        .collect()
}

fn undo_edits(engine: &Engine) -> Vec<u64> {
    engine
        .undo_entries()
        .map(|entry| entry.edit().get())
        .collect()
}

fn redo_edits(engine: &Engine) -> Vec<u64> {
    engine
        .redo_entries()
        .map(|entry| entry.edit().get())
        .collect()
}

#[test]
fn add_and_remove_are_project_edits() {
    assert_eq!(
        add(4, "New", EngineInputSource::Bars).kind(),
        EngineCommandKind::ProjectEdit
    );
    assert_eq!(remove(3).kind(), EngineCommandKind::ProjectEdit);
}

#[test]
fn added_input_is_appended_with_a_default_strip_and_becomes_switchable() {
    let mut engine = engine();
    let outcome = run(&mut engine, "add", add(4, "Red", RED));
    assert_accepted(&outcome);
    assert_eq!(
        outcome
            .events
            .iter()
            .map(|event| event.payload.clone())
            .collect::<Vec<_>>(),
        vec![EngineEvent::InputAdded {
            input: input(4),
            name: "Red".into(),
            source: RED,
        }]
    );
    assert_eq!(ids(&engine), [1, 2, 3, 4]);
    assert_eq!(engine.show().input_name(input(4)), Some("Red"));
    assert_eq!(
        engine.show().input_audio_strip(input(4)),
        Some(EngineInputAudioStripState::default())
    );
    assert_eq!(
        engine.realized_switcher().inputs(),
        engine.show().inputs(),
        "the realized switcher learns the input at the next tick"
    );

    assert_accepted(&run(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(4)),
    ));
    assert_accepted(&run(&mut engine, "cut", EngineCommand::Cut));
    assert_eq!(engine.realized_switcher().program(), input(4));

    // An idle snapshot restores with the grown catalogue.
    let restored = Engine::restore(engine.snapshot().unwrap()).unwrap();
    assert_eq!(restored.show(), engine.show());
}

#[test]
fn add_refuses_duplicate_identity_and_names_and_the_project_padlock() {
    let mut engine = engine();
    let duplicate_id = run(&mut engine, "dup-id", add(2, "Other", RED));
    assert_eq!(rejection(&duplicate_id).code, RejectionCode::Conflict);
    assert_eq!(issue(&duplicate_id), "duplicate_input");

    let duplicate_name = run(&mut engine, "dup-name", add(4, "Input 1", RED));
    assert_eq!(rejection(&duplicate_name).code, RejectionCode::Conflict);
    assert_eq!(issue(&duplicate_name), "duplicate_name");

    let blank = run(&mut engine, "blank", add(4, "  ", RED));
    assert_eq!(rejection(&blank).code, RejectionCode::InvalidCommand);

    assert_accepted(&run(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    let locked = run(&mut engine, "locked", add(4, "Red", RED));
    assert_eq!(
        EditRejectionKind::of(rejection(&locked)),
        Some(EditRejectionKind::ProjectLocked)
    );
    assert_eq!(ids(&engine), [1, 2, 3]);
}

#[test]
fn add_refuses_beyond_the_input_bound() {
    let inputs = (1..=MAX_SHOW_INPUTS as u128)
        .map(|value| (input(value), format!("Input {value}")))
        .collect();
    let mut engine = Engine::new(
        ShowState::new("full", inputs, input(1), input(2)).unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    let full = run(&mut engine, "full", add(10_000, "One more", RED));
    assert_eq!(issue(&full), "too_many_inputs");
}

#[test]
fn remove_refuses_inputs_on_air_configured_or_locked() {
    let mut engine = engine();
    for (key, value) in [("program", 1), ("preview", 2)] {
        let outcome = run(&mut engine, key, remove(value));
        assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
        assert_eq!(issue(&outcome), "input_in_use");
    }
    let unknown = run(&mut engine, "unknown", remove(9));
    assert_eq!(rejection(&unknown).code, RejectionCode::NotFound);

    let channel = OverlayChannelId::new(1).unwrap();
    assert_accepted(&run(
        &mut engine,
        "overlay",
        EngineCommand::TakeOverlay {
            channel,
            source: input(3),
        },
    ));
    let overlay = run(&mut engine, "overlay-remove", remove(3));
    assert!(rejection(&overlay).message.contains("overlay 1"));
    assert_accepted(&run(
        &mut engine,
        "overlay-off",
        EngineCommand::OverlayOff { channel },
    ));

    let slot = StingerSlotId::new(1).unwrap();
    assert_accepted(&run(
        &mut engine,
        "stinger",
        EngineCommand::ConfigureStinger {
            slot,
            descriptor: StingerDescriptor::new(
                input(3),
                false,
                2,
                StingerAudioPolicy::Muted,
                MissingMediaFallback::Cut,
            ),
        },
    ));
    let stinger = run(&mut engine, "stinger-remove", remove(3));
    assert!(
        rejection(&stinger).message.contains("Stinger 1"),
        "{:?}",
        stinger.receipt
    );
    assert_accepted(&run(
        &mut engine,
        "stinger-clear",
        EngineCommand::RemoveStinger { slot },
    ));

    assert_accepted(&run(
        &mut engine,
        "lock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(3)),
            locked: true,
        },
    ));
    let input_locked = run(&mut engine, "locked-remove", remove(3));
    assert_eq!(
        EditRejectionKind::of(rejection(&input_locked)),
        Some(EditRejectionKind::InputLocked)
    );
    assert_accepted(&run(
        &mut engine,
        "unlock-input",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(3)),
            locked: false,
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "lock-project",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    let project_locked = run(&mut engine, "project-locked-remove", remove(3));
    assert_eq!(
        EditRejectionKind::of(rejection(&project_locked)),
        Some(EditRejectionKind::ProjectLocked)
    );
    assert_eq!(ids(&engine), [1, 2, 3]);
}

#[test]
fn remove_refuses_an_input_the_realized_switcher_still_shows() {
    let mut engine = engine();
    // Preview moves to 3 in the desired switcher; the realized switcher still
    // shows 2 until the next tick, so 2 cannot be removed yet.
    let preview = engine
        .execute(
            envelope("preview", EngineCommand::SelectPreview(input(3))),
            0,
        )
        .unwrap();
    assert_accepted(&preview);
    let early = engine.execute(envelope("early", remove(2)), 0).unwrap();
    assert!(rejection(&early).message.contains("on Preview"));
    engine.tick().unwrap();
    assert_accepted(&run(&mut engine, "late", remove(2)));
    assert_eq!(ids(&engine), [1, 3]);
}

#[test]
fn removed_input_loses_its_strip_and_leaves_both_switchers() {
    let mut engine = engine();
    let outcome = run(&mut engine, "remove", remove(3));
    assert_accepted(&outcome);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::InputRemoved { input: input(3) }
    );
    assert_eq!(ids(&engine), [1, 2]);
    assert_eq!(engine.show().input_audio_strip(input(3)), None);
    assert_eq!(engine.realized_switcher().inputs(), [input(1), input(2)]);
    let restored = Engine::restore(engine.snapshot().unwrap()).unwrap();
    assert_eq!(restored.show(), engine.show());
}

#[test]
fn undo_of_an_add_removes_it_and_redo_restores_it_exactly() {
    let mut engine = engine();
    let added = run(&mut engine, "add", add(4, "Bars", EngineInputSource::Bars));
    let edit = added.receipt.accepted().unwrap().revision.get();
    assert_eq!(undo_edits(&engine), [edit]);
    // A strip move is a live operation: it is not recorded and undo
    // discards it with the input.
    assert_accepted(&run(
        &mut engine,
        "strip",
        EngineCommand::SetInputAudioStrip {
            input: input(4),
            state: EngineInputAudioStripState {
                muted: true,
                ..EngineInputAudioStripState::default()
            },
        },
    ));

    let undone = run(&mut engine, "undo", UNDO);
    assert_accepted(&undone);
    assert_eq!(
        undone.events[0].payload,
        EngineEvent::InputRemoved { input: input(4) }
    );
    assert_eq!(ids(&engine), [1, 2, 3]);
    assert_eq!(redo_edits(&engine), [edit]);

    let redone = run(&mut engine, "redo", REDO);
    assert_accepted(&redone);
    assert_eq!(
        redone.events[0].payload,
        EngineEvent::InputAdded {
            input: input(4),
            name: "Bars".into(),
            source: EngineInputSource::Bars,
        }
    );
    assert_eq!(ids(&engine), [1, 2, 3, 4]);
    assert_eq!(
        engine.show().input_audio_strip(input(4)),
        Some(EngineInputAudioStripState::default())
    );
    assert_eq!(undo_edits(&engine), [edit]);
    assert_eq!(redo_edits(&engine), Vec::<u64>::new());
}

#[test]
fn undo_of_an_add_is_refused_while_the_input_is_on_air() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "add", add(4, "Red", RED)));
    assert_accepted(&run(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(4)),
    ));
    let refused = run(&mut engine, "undo", UNDO);
    assert_eq!(rejection(&refused).code, RejectionCode::Conflict);
    assert_eq!(issue(&refused), "input_in_use");
    assert_eq!(ids(&engine), [1, 2, 3, 4]);
    assert_eq!(undo_edits(&engine).len(), 1);
}

#[test]
fn undo_of_an_add_is_drift_when_the_input_no_longer_ends_the_order() {
    let mut engine = engine();
    let added = run(&mut engine, "add", add(4, "Red", RED));
    let edit = added.receipt.accepted().unwrap().revision.get();
    // Another session's reorder is undone and the add is on top again, but
    // the input is now first: a redo could not put it back there.
    assert_accepted(&run(
        &mut engine,
        "reorder",
        EngineCommand::ReorderInputs {
            inputs: vec![input(4), input(1), input(2), input(3)],
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "remove-other",
        EngineCommand::RemoveInput { input: input(3) },
    ));
    // The reorder named input 3 and was forgotten with it; the add remains.
    assert_eq!(undo_edits(&engine), [edit]);
    let drift = run(&mut engine, "undo", UNDO);
    assert_eq!(
        EditRejectionKind::of(rejection(&drift)),
        Some(EditRejectionKind::HistoryDrift)
    );
}

#[test]
fn remove_is_not_undoable_and_forgets_history_naming_the_input() {
    let mut engine = engine();
    let rename = |value: u128, name: &str| EngineCommand::RenameInput {
        input: input(value),
        name: name.to_owned(),
    };
    let first = run(&mut engine, "r1", rename(1, "A"));
    let first = first.receipt.accepted().unwrap().revision.get();
    assert_accepted(&run(&mut engine, "r3", rename(3, "C")));
    assert_accepted(&run(&mut engine, "r2", rename(2, "B")));
    assert_accepted(&run(&mut engine, "undo", UNDO));
    assert_eq!(redo_edits(&engine).len(), 1);

    assert_accepted(&run(&mut engine, "remove", remove(3)));
    assert_eq!(undo_edits(&engine), [first], "the rename of 3 is forgotten");
    assert!(redo_edits(&engine).is_empty(), "a new edit clears redo");

    // Undo reverts the surviving rename, never the removal.
    assert_accepted(&run(&mut engine, "undo-2", UNDO));
    assert_eq!(engine.show().input_name(input(1)), Some("Input 1"));
    assert_eq!(ids(&engine), [1, 2]);
    let empty = run(&mut engine, "undo-3", UNDO);
    assert_eq!(
        EditRejectionKind::of(rejection(&empty)),
        Some(EditRejectionKind::NothingToUndo)
    );
}

#[test]
fn removal_releases_the_source_a_settled_off_overlay_retains() {
    let mut engine = engine();
    let channel = OverlayChannelId::new(2).unwrap();
    assert_accepted(&run(
        &mut engine,
        "take",
        EngineCommand::TakeOverlay {
            channel,
            source: input(3),
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "off",
        EngineCommand::OverlayOff { channel },
    ));
    assert_eq!(
        engine.show().desired_switcher().overlay(channel).source(),
        Some(input(3))
    );
    assert_accepted(&run(&mut engine, "remove", remove(3)));
    assert_eq!(
        engine.show().desired_switcher().overlay(channel).source(),
        None
    );
    assert_eq!(engine.realized_switcher().overlay(channel).source(), None);
}
