//! SW-017: bounded undo/redo of project edits and the edit padlock.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode, Revision};
use fm_engine::{
    EditLockScope, EditRejectionKind, Engine, EngineCommand, EngineCommandKind,
    EngineCommandOutcome, EngineEvent, EngineInputAudioStripState, EngineRestoreState,
    MAX_EDIT_HISTORY, ShowState,
};
use fm_scheduler::FrameNumber;
use fm_switcher::{
    MissingMediaFallback, OverlayBorderPreset, OverlayChannelId, OverlayPositionPreset,
    OverlayTransitionKind, RecorderSlot, StingerAudioPolicy, StingerDescriptor, StingerSlotId,
};
use fm_types::{FrameRate, InputId, OutputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn output(value: u128) -> OutputId {
    OutputId::new(NonZeroU128::new(value).unwrap())
}

fn show() -> ShowState {
    ShowState::new(
        "undo show",
        [1, 2, 3]
            .into_iter()
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap()
    .with_outputs(vec![(output(9), "Program".into())])
    .unwrap()
}

fn engine() -> Engine {
    Engine::new(
        show(),
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

fn rename(input_value: u128, name: &str) -> EngineCommand {
    EngineCommand::RenameInput {
        input: input(input_value),
        name: name.to_owned(),
    }
}

const UNDO: EngineCommand = EngineCommand::Undo {
    expected_edit: None,
};
const REDO: EngineCommand = EngineCommand::Redo {
    expected_edit: None,
};

fn lock(scope: EditLockScope, locked: bool) -> EngineCommand {
    EngineCommand::SetEditLock { scope, locked }
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

fn edit_rejection(outcome: &EngineCommandOutcome) -> EditRejectionKind {
    EditRejectionKind::of(rejection(outcome)).expect("rejection should be an edit rejection")
}

fn assert_accepted(outcome: &EngineCommandOutcome) {
    assert!(
        outcome.receipt.accepted().is_some(),
        "expected acceptance, got {:?}",
        outcome.receipt
    );
}

fn names(engine: &Engine) -> Vec<String> {
    engine.show().input_names().to_vec()
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

fn stinger_descriptor(media: u128) -> StingerDescriptor {
    StingerDescriptor::new(
        input(media),
        false,
        2,
        StingerAudioPolicy::MixWithProgram,
        MissingMediaFallback::Cut,
    )
}

#[test]
fn command_kinds_separate_edits_from_live_operations() {
    let channel = OverlayChannelId::new(1).unwrap();
    for edit in [
        rename(1, "A"),
        EngineCommand::ReorderInputs { inputs: vec![] },
        EngineCommand::ConfigureStinger {
            slot: StingerSlotId::new(1).unwrap(),
            descriptor: stinger_descriptor(3),
        },
        EngineCommand::RemoveStinger {
            slot: StingerSlotId::new(1).unwrap(),
        },
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition: OverlayTransitionKind::Fade,
            duration_frames: 3,
        },
        EngineCommand::ConfigureOverlayAppearance {
            channel,
            position: OverlayPositionPreset::FullFrame,
            border: OverlayBorderPreset::None,
        },
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output: output(9),
            included: true,
        },
    ] {
        assert_eq!(edit.kind(), EngineCommandKind::ProjectEdit, "{edit:?}");
    }
    for live in [
        EngineCommand::Cut,
        EngineCommand::Fade { duration_frames: 2 },
        EngineCommand::SelectPreview(input(3)),
        EngineCommand::TakeOverlay {
            channel,
            source: input(3),
        },
        EngineCommand::FadeToBlack {
            active: true,
            duration_frames: 2,
        },
        EngineCommand::RecordStart {
            recorder: RecorderSlot::ALL[0],
        },
        EngineCommand::SetInputAudioStrip {
            input: input(1),
            state: EngineInputAudioStripState::default(),
        },
    ] {
        assert_eq!(live.kind(), EngineCommandKind::LiveOperation, "{live:?}");
    }
    assert_eq!(UNDO.kind(), EngineCommandKind::History);
    assert_eq!(REDO.kind(), EngineCommandKind::History);
    assert_eq!(
        lock(EditLockScope::Project, true).kind(),
        EngineCommandKind::EditLock
    );
}

#[test]
fn undo_and_redo_walk_a_chain_of_mixed_edits() {
    let mut engine = engine();
    let channel = OverlayChannelId::new(1).unwrap();
    let original_order = engine.show().inputs().to_vec();

    assert_accepted(&run(&mut engine, "rename", rename(1, "Camera")));
    assert_accepted(&run(
        &mut engine,
        "reorder",
        EngineCommand::ReorderInputs {
            inputs: vec![input(3), input(1), input(2)],
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "overlay-transition",
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition: OverlayTransitionKind::Fade,
            duration_frames: 5,
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "inclusion",
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output: output(9),
            included: true,
        },
    ));
    assert_eq!(undo_edits(&engine), [1, 2, 3, 4]);

    // Undo the inclusion and the overlay transition.
    assert_accepted(&run(&mut engine, "undo-1", UNDO));
    assert!(
        !engine
            .show()
            .desired_switcher()
            .overlay(channel)
            .is_included_in(output(9))
    );
    assert!(
        !engine
            .realized_switcher()
            .overlay(channel)
            .is_included_in(output(9))
    );
    assert_accepted(&run(&mut engine, "undo-2", UNDO));
    let overlay = engine.show().desired_switcher().overlay(channel);
    assert_eq!(overlay.transition(), OverlayTransitionKind::Cut);

    // Undoing the reorder emits the ordinary order event and realizes it.
    let outcome = run(&mut engine, "undo-3", UNDO);
    assert_accepted(&outcome);
    assert_eq!(outcome.events.len(), 1);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::InputOrderChanged {
            inputs: original_order.clone()
        }
    );
    assert_eq!(engine.show().inputs(), original_order);
    assert_eq!(engine.realized_switcher().inputs(), original_order);

    let outcome = run(&mut engine, "undo-4", UNDO);
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::InputRenamed {
            input: input(1),
            name: "Input 1".into()
        }
    );
    assert_eq!(names(&engine), ["Input 1", "Input 2", "Input 3"]);
    assert_eq!(undo_edits(&engine), [] as [u64; 0]);
    assert_eq!(redo_edits(&engine), [4, 3, 2, 1]);
    let empty = run(&mut engine, "undo-empty", UNDO);
    assert_eq!(edit_rejection(&empty), EditRejectionKind::NothingToUndo);
    assert_eq!(
        empty.receipt.rejected().unwrap().rejection.reason,
        Some(fm_command::RefusalReason::NothingToUndo)
    );

    // Redo the rename and the reorder; entries keep their original identity.
    assert_accepted(&run(&mut engine, "redo-1", REDO));
    assert_accepted(&run(&mut engine, "redo-2", REDO));
    assert_eq!(names(&engine), ["Input 3", "Camera", "Input 2"]);
    assert_eq!(engine.show().inputs(), [input(3), input(1), input(2)]);
    assert_eq!(undo_edits(&engine), [1, 2]);
    assert_eq!(redo_edits(&engine), [4, 3]);

    // Every accepted history command kept durable counters in lockstep, so an
    // idle snapshot still restores.
    let snapshot = engine.snapshot().unwrap();
    assert_eq!(snapshot.revision().get(), snapshot.event_sequence().get());
    assert_eq!(
        snapshot.revision().get(),
        snapshot.runtime_generation().get()
    );
    Engine::restore(snapshot).unwrap();
}

#[test]
fn undo_reverts_stinger_configuration_to_its_exact_prior_state() {
    let mut engine = engine();
    let slot = StingerSlotId::new(1).unwrap();
    assert_accepted(&run(
        &mut engine,
        "configure",
        EngineCommand::ConfigureStinger {
            slot,
            descriptor: stinger_descriptor(3),
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "reconfigure",
        EngineCommand::ConfigureStinger {
            slot,
            descriptor: stinger_descriptor(2),
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "remove",
        EngineCommand::RemoveStinger { slot },
    ));
    assert!(
        engine
            .show()
            .desired_switcher()
            .stinger(slot)
            .descriptor()
            .is_none()
    );

    assert_accepted(&run(&mut engine, "undo-remove", UNDO));
    assert_eq!(
        engine.show().desired_switcher().stinger(slot).descriptor(),
        Some(&stinger_descriptor(2))
    );
    assert_accepted(&run(&mut engine, "undo-reconfigure", UNDO));
    assert_eq!(
        engine.realized_switcher().stinger(slot).descriptor(),
        Some(&stinger_descriptor(3))
    );
    assert_accepted(&run(&mut engine, "undo-configure", UNDO));
    assert!(
        engine
            .realized_switcher()
            .stinger(slot)
            .descriptor()
            .is_none()
    );
}

#[test]
fn undo_never_reverts_live_switching_or_recording() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename", rename(2, "Guest")));
    assert_accepted(&run(&mut engine, "cut", EngineCommand::Cut));
    assert_accepted(&run(
        &mut engine,
        "record",
        EngineCommand::RecordStart {
            recorder: RecorderSlot::ALL[0],
        },
    ));
    assert_eq!(engine.show().desired_switcher().program(), input(2));
    // Live operations are not recorded.
    assert_eq!(undo_edits(&engine), [1]);

    assert_accepted(&run(&mut engine, "undo", UNDO));
    assert_eq!(names(&engine)[1], "Input 2");
    assert_eq!(engine.show().desired_switcher().program(), input(2));
    assert_eq!(engine.realized_switcher().program(), input(2));
    assert!(
        engine
            .show()
            .desired_switcher()
            .recording_desired(RecorderSlot::ALL[0])
    );

    // With only live operations left, there is nothing to undo.
    assert_eq!(
        edit_rejection(&run(&mut engine, "undo-again", UNDO)),
        EditRejectionKind::NothingToUndo
    );
    assert_eq!(engine.show().desired_switcher().program(), input(2));
}

#[test]
fn live_operations_keep_redo_and_a_new_edit_clears_it() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename-1", rename(1, "One")));
    assert_accepted(&run(&mut engine, "rename-2", rename(2, "Two")));
    assert_accepted(&run(&mut engine, "undo", UNDO));
    assert_eq!(redo_edits(&engine), [2]);

    assert_accepted(&run(&mut engine, "cut", EngineCommand::Cut));
    assert_accepted(&run(
        &mut engine,
        "lock",
        lock(EditLockScope::Input(input(3)), true),
    ));
    assert_eq!(redo_edits(&engine), [2]);

    assert_accepted(&run(&mut engine, "rename-3", rename(1, "Uno")));
    assert_eq!(redo_edits(&engine), [] as [u64; 0]);
    assert_eq!(
        edit_rejection(&run(&mut engine, "redo", REDO)),
        EditRejectionKind::NothingToRedo
    );
    assert_eq!(names(&engine), ["Uno", "Input 2", "Input 3"]);
}

#[test]
fn undo_is_idempotent_by_key() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename-1", rename(1, "One")));
    assert_accepted(&run(&mut engine, "rename-2", rename(1, "Uno")));

    let first = run(&mut engine, "undo-key", UNDO);
    assert_accepted(&first);
    let revision = engine.revision();
    let replay = engine.execute(envelope("undo-key", UNDO), 0).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(engine.revision(), revision);
    assert_eq!(names(&engine)[0], "One");
    assert_eq!(undo_edits(&engine), [1]);
}

#[test]
fn expected_edit_rejects_reverting_another_editors_later_edit() {
    let mut engine = engine();
    // Editor A renames input 1 (edit 1); editor B then renames input 2 (edit 2).
    let a = run(&mut engine, "editor-a", rename(1, "Alpha"));
    let a_edit = a.receipt.accepted().unwrap().revision;
    let b = run(&mut engine, "editor-b", rename(2, "Bravo"));
    let b_edit = b.receipt.accepted().unwrap().revision;
    let before = engine.show().clone();

    // A asks to undo its own edit, but B's later edit is on top.
    let outcome = run(
        &mut engine,
        "a-undo",
        EngineCommand::Undo {
            expected_edit: Some(a_edit),
        },
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(edit_rejection(&outcome), EditRejectionKind::HistoryConflict);
    assert_eq!(engine.show(), &before);
    assert_eq!(undo_edits(&engine), [a_edit.get(), b_edit.get()]);

    // B can undo its own edit; then A's expectation matches.
    assert_accepted(&run(
        &mut engine,
        "b-undo",
        EngineCommand::Undo {
            expected_edit: Some(b_edit),
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "a-undo-retry",
        EngineCommand::Undo {
            expected_edit: Some(a_edit),
        },
    ));
    assert_eq!(names(&engine), ["Input 1", "Input 2", "Input 3"]);

    // Redo enforces the same expectation against the redo stack top.
    let outcome = run(
        &mut engine,
        "b-redo-wrong",
        EngineCommand::Redo {
            expected_edit: Some(b_edit),
        },
    );
    assert_eq!(edit_rejection(&outcome), EditRejectionKind::HistoryConflict);
    assert_accepted(&run(
        &mut engine,
        "a-redo",
        EngineCommand::Redo {
            expected_edit: Some(a_edit),
        },
    ));
    assert_eq!(names(&engine)[0], "Alpha");
}

#[test]
fn stale_expected_revision_rejects_undo_before_resolution() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename", rename(1, "One")));
    let outcome = run(
        &mut engine,
        "undo-stale",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert_accepted(&outcome);
    let outcome = engine
        .execute(envelope("redo-stale", REDO).expecting(Revision::new(1)), 0)
        .unwrap();
    assert_eq!(rejection(&outcome).code, RejectionCode::RevisionConflict);
    assert_eq!(redo_edits(&engine), [1]);
}

#[test]
#[allow(clippy::too_many_lines)]
fn project_padlock_blocks_edits_but_not_live_switching() {
    let mut engine = engine();
    let channel = OverlayChannelId::new(1).unwrap();
    assert_accepted(&run(&mut engine, "rename", rename(1, "Camera")));
    let outcome = run(&mut engine, "lock", lock(EditLockScope::Project, true));
    assert_eq!(
        outcome.events[0].payload,
        EngineEvent::EditLockChanged {
            scope: EditLockScope::Project,
            locked: true
        }
    );
    assert!(engine.show().is_project_locked());
    let locked_show_names = names(&engine);

    for (key, command) in [
        ("rename", rename(2, "Guest")),
        (
            "reorder",
            EngineCommand::ReorderInputs {
                inputs: vec![input(2), input(1), input(3)],
            },
        ),
        (
            "stinger",
            EngineCommand::ConfigureStinger {
                slot: StingerSlotId::new(1).unwrap(),
                descriptor: stinger_descriptor(3),
            },
        ),
        (
            "overlay-appearance",
            EngineCommand::ConfigureOverlayAppearance {
                channel,
                position: OverlayPositionPreset::FullFrame,
                border: OverlayBorderPreset::None,
            },
        ),
        ("undo", UNDO),
        ("redo", REDO),
    ] {
        let outcome = run(&mut engine, &format!("locked-{key}"), command);
        assert_eq!(rejection(&outcome).code, RejectionCode::Conflict, "{key}");
        assert_eq!(
            edit_rejection(&outcome),
            EditRejectionKind::ProjectLocked,
            "{key}"
        );
    }
    assert_eq!(names(&engine), locked_show_names);
    assert_eq!(undo_edits(&engine), [1]);

    // Live switching, overlays, audio, recording, and FTB remain available.
    assert_accepted(&run(&mut engine, "cut", EngineCommand::Cut));
    assert_eq!(engine.realized_switcher().program(), input(2));
    assert_accepted(&run(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(3)),
    ));
    let outcome = engine
        .execute(
            envelope("fade", EngineCommand::Fade { duration_frames: 2 }),
            0,
        )
        .unwrap();
    assert_accepted(&outcome);
    for _ in 0..3 {
        engine.tick().unwrap();
    }
    assert_eq!(engine.realized_switcher().program(), input(3));
    assert_accepted(&run(
        &mut engine,
        "overlay",
        EngineCommand::TakeOverlay {
            channel,
            source: input(1),
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "audio",
        EngineCommand::SetInputAudioStrip {
            input: input(1),
            state: EngineInputAudioStripState {
                muted: true,
                ..EngineInputAudioStripState::default()
            },
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "record",
        EngineCommand::RecordStart {
            recorder: RecorderSlot::ALL[0],
        },
    ));

    // Unlocking restores editing, and the pre-lock history is still usable.
    assert_accepted(&run(
        &mut engine,
        "unlock",
        lock(EditLockScope::Project, false),
    ));
    assert_accepted(&run(&mut engine, "undo-after-unlock", UNDO));
    assert_eq!(names(&engine)[0], "Input 1");
    assert_eq!(engine.realized_switcher().program(), input(3));
}

#[test]
fn input_padlock_blocks_edits_to_that_input_only() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename", rename(1, "Camera")));
    assert_accepted(&run(
        &mut engine,
        "lock-1",
        lock(EditLockScope::Input(input(1)), true),
    ));
    assert!(engine.show().is_input_locked(input(1)));
    assert!(!engine.show().is_input_locked(input(2)));

    let outcome = run(&mut engine, "rename-locked", rename(1, "Other"));
    assert_eq!(edit_rejection(&outcome), EditRejectionKind::InputLocked);
    assert_accepted(&run(&mut engine, "rename-unlocked", rename(2, "Guest")));

    // Reordering may not move a locked input but may move others around it.
    let outcome = run(
        &mut engine,
        "reorder-moves-locked",
        EngineCommand::ReorderInputs {
            inputs: vec![input(2), input(1), input(3)],
        },
    );
    assert_eq!(edit_rejection(&outcome), EditRejectionKind::InputLocked);
    assert_accepted(&run(
        &mut engine,
        "reorder-around-locked",
        EngineCommand::ReorderInputs {
            inputs: vec![input(1), input(3), input(2)],
        },
    ));

    // Undo walks back the reorder and the unlocked rename, then refuses to
    // revert the locked input's rename without consuming history.
    assert_accepted(&run(&mut engine, "undo-reorder", UNDO));
    assert_accepted(&run(&mut engine, "undo-guest", UNDO));
    let outcome = run(&mut engine, "undo-camera", UNDO);
    assert_eq!(edit_rejection(&outcome), EditRejectionKind::InputLocked);
    assert_eq!(names(&engine)[0], "Camera");
    assert_eq!(undo_edits(&engine), [1]);

    // Live operations involving the locked input still work.
    assert_accepted(&run(
        &mut engine,
        "preview-locked",
        EngineCommand::SelectPreview(input(3)),
    ));
    assert_accepted(&run(&mut engine, "cut", EngineCommand::Cut));
    assert_eq!(engine.realized_switcher().program(), input(3));

    let outcome = run(
        &mut engine,
        "lock-unknown",
        lock(EditLockScope::Input(input(99)), true),
    );
    assert_eq!(rejection(&outcome).code, RejectionCode::NotFound);

    assert_accepted(&run(
        &mut engine,
        "unlock-1",
        lock(EditLockScope::Input(input(1)), false),
    ));
    assert_accepted(&run(&mut engine, "undo-camera-unlocked", UNDO));
    assert_eq!(names(&engine)[0], "Input 1");
}

#[test]
fn history_is_bounded_and_discards_the_oldest_edit() {
    let mut engine = engine();
    let total = MAX_EDIT_HISTORY + 5;
    for index in 0..total {
        assert_accepted(&run(
            &mut engine,
            &format!("rename-{index}"),
            rename(1, &format!("Name {index}")),
        ));
    }
    assert_eq!(engine.undo_entries().len(), MAX_EDIT_HISTORY);
    let first_kept = u64::try_from(total - MAX_EDIT_HISTORY + 1).unwrap();
    assert_eq!(
        engine.undo_entries().next().unwrap().edit(),
        Revision::new(first_kept)
    );

    for index in 0..MAX_EDIT_HISTORY {
        assert_accepted(&run(&mut engine, &format!("undo-{index}"), UNDO));
    }
    assert_eq!(
        edit_rejection(&run(&mut engine, "undo-past-bound", UNDO)),
        EditRejectionKind::NothingToUndo
    );
    // The oldest retained edit was "Name 5"; its inverse restores "Name 4".
    assert_eq!(
        names(&engine)[0],
        format!("Name {}", total - MAX_EDIT_HISTORY - 1)
    );
    assert_eq!(engine.redo_entries().len(), MAX_EDIT_HISTORY);
}

#[test]
fn restore_keeps_padlocks_and_resets_history() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename-1", rename(1, "One")));
    assert_accepted(&run(&mut engine, "rename-2", rename(2, "Two")));
    assert_accepted(&run(&mut engine, "undo", UNDO));
    assert_accepted(&run(
        &mut engine,
        "lock-3",
        lock(EditLockScope::Input(input(3)), true),
    ));
    assert_eq!(undo_edits(&engine), [1]);
    assert_eq!(redo_edits(&engine), [2]);

    let snapshot = engine.snapshot().unwrap();
    let mut restored = Engine::restore(snapshot.clone()).unwrap();
    assert_eq!(restored.undo_entries().len(), 0);
    assert_eq!(restored.redo_entries().len(), 0);
    assert!(restored.show().is_input_locked(input(3)));
    assert_eq!(
        edit_rejection(&run(&mut restored, "undo-after-restore", UNDO)),
        EditRejectionKind::NothingToUndo
    );
    assert_eq!(
        edit_rejection(&run(&mut restored, "redo-after-restore", REDO)),
        EditRejectionKind::NothingToRedo
    );
    assert_eq!(
        edit_rejection(&run(&mut restored, "rename-locked", rename(3, "Three"))),
        EditRejectionKind::InputLocked
    );

    // A restore from persisted parts takes padlocks from the supplied show.
    let persisted = Engine::restore_persisted(
        snapshot.show().clone(),
        snapshot.realized_switcher().clone(),
        snapshot.frame_rate(),
        snapshot.clock_domain(),
        EngineRestoreState {
            state_epoch: snapshot.state_epoch(),
            revision: snapshot.revision(),
            event_sequence: snapshot.event_sequence(),
            runtime_generation: snapshot.runtime_generation(),
            clock_time: snapshot.clock_time(),
            frame_cursor: FrameNumber::new(snapshot.frames_rendered()),
            receipts: snapshot.receipts().to_vec(),
        },
    )
    .unwrap();
    assert_eq!(persisted.undo_entries().len(), 0);
    assert!(persisted.show().is_input_locked(input(3)));
}

#[test]
fn aborted_preparation_leaves_history_untouched() {
    let mut engine = engine();
    assert_accepted(&run(&mut engine, "rename", rename(1, "One")));
    let prepared = engine
        .prepare_execute(envelope("undo", UNDO), 0)
        .unwrap()
        .prepared()
        .unwrap();
    assert_eq!(prepared.staged_engine().undo_entries().len(), 0);
    assert_eq!(prepared.staged_engine().redo_entries().len(), 1);
    prepared.abort();
    assert_eq!(undo_edits(&engine), [1]);
    assert_eq!(redo_edits(&engine), [] as [u64; 0]);

    let prepared = engine
        .prepare_execute(envelope("undo", UNDO), 0)
        .unwrap()
        .prepared()
        .unwrap();
    engine.commit_execute(prepared).unwrap();
    assert_eq!(redo_edits(&engine), [1]);
}

#[test]
fn undo_of_stinger_edit_waits_for_an_in_flight_transition() {
    let mut engine = engine();
    let slot = StingerSlotId::new(1).unwrap();
    assert_accepted(&run(
        &mut engine,
        "configure",
        EngineCommand::ConfigureStinger {
            slot,
            descriptor: stinger_descriptor(3),
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "fade",
        EngineCommand::Fade { duration_frames: 4 },
    ));
    let outcome = run(&mut engine, "undo-busy", UNDO);
    assert_eq!(rejection(&outcome).code, RejectionCode::Conflict);
    assert_eq!(undo_edits(&engine), [1]);
    for _ in 0..4 {
        engine.tick().unwrap();
    }
    assert_accepted(&run(&mut engine, "undo-idle", UNDO));
    assert!(
        engine
            .show()
            .desired_switcher()
            .stinger(slot)
            .descriptor()
            .is_none()
    );
}
