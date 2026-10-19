//! Project scripts (RC-011): a run performs its actions as ordinary show
//! commands on one frame, all or nothing; syntax and runtime errors,
//! exhausted budgets, and refused actions refuse the run with a typed
//! reason naming the line and column; script edits are undoable and
//! padlocked; a run leaves the undo history alone.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{
    CommandEnvelope, IdKind, IdempotencyKey, RefusalReason, RejectionCode, ScriptFailureKind,
};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandOutcome, EngineEvent,
    EngineScriptDefinition, FrameResult, ShowState,
};
use fm_switcher::{OverlayChannelId, SwitcherEvent};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "script show",
            [11, 12, 13, 14]
                .into_iter()
                .enumerate()
                .map(|(index, value)| (input(value), format!("Camera {}", index + 1)))
                .collect(),
            input(11),
            input(14),
        )
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn set(script: u64, source: &str) -> EngineCommand {
    EngineCommand::SetScript {
        script,
        definition: Some(EngineScriptDefinition {
            name: format!("Script {script}"),
            source: source.to_owned(),
        }),
    }
}

fn run(script: u64) -> EngineCommand {
    EngineCommand::RunScript { script }
}

fn submit(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accepted(outcome: &EngineCommandOutcome) -> u64 {
    outcome
        .receipt
        .accepted()
        .unwrap_or_else(|| panic!("command was rejected: {:?}", outcome.receipt))
        .result
        .target_frame
        .get()
}

fn refusal(outcome: &EngineCommandOutcome) -> (RejectionCode, Option<RefusalReason>, String) {
    let rejection = &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection;
    (
        rejection.code,
        rejection.reason.clone(),
        rejection.message.clone(),
    )
}

fn failed(failure: ScriptFailureKind, line: u32, column: u32) -> RefusalReason {
    RefusalReason::ScriptFailed {
        failure,
        line,
        column,
    }
}

fn tick(engine: &mut Engine, frames: u64) -> Vec<FrameResult> {
    (0..frames).map(|_| engine.tick().unwrap()).collect()
}

fn program_changes(frames: &[FrameResult]) -> Vec<(u64, InputId)> {
    frames
        .iter()
        .flat_map(|frame| {
            frame.events.iter().filter_map(move |event| match event {
                SwitcherEvent::ProgramChanged { program, .. } => {
                    Some((frame.frame.get(), *program))
                }
                _ => None,
            })
        })
        .collect()
}

#[test]
fn a_run_applies_every_action_on_one_frame() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            "# take the camera after Program, by number and by name\n\
             let next = program + 1\n\
             preview next\n\
             cut\n\
             if program == 2\n\
             \x20 overlay 1, \"Camera 4\"\n\
             end\n\
             mute 3\n",
        ),
    ));
    let outcome = submit(&mut engine, "run", run(1));
    let frame = accepted(&outcome);
    let ran = outcome
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EngineEvent::ScriptRan {
                script,
                frame: ran_frame,
                actions,
                instructions,
                applied,
                ..
            } => Some((*script, *ran_frame, *actions, *instructions, applied.len())),
            _ => None,
        })
        .expect("the run is announced");
    assert_eq!((ran.0, ran.1, ran.2), (1, frame, 4));
    assert!(ran.3 > 0);
    assert!(ran.4 >= 4);

    // The desired show already holds every action.
    let switcher = engine.show().desired_switcher();
    assert_eq!(switcher.program(), input(12));
    assert!(engine.show().input_audio_strip(input(13)).unwrap().muted);
    let channel = OverlayChannelId::new(1).unwrap();
    assert_eq!(switcher.overlay(channel).source(), Some(input(14)));

    // They are realized together on the run's frame.
    let frames = tick(&mut engine, 3);
    assert_eq!(program_changes(&frames), vec![(frame, input(12))]);
}

#[test]
fn a_failed_run_changes_nothing_and_says_where() {
    let mut engine = engine();
    let before = engine.show().clone();
    for (script, source, code, reason) in [
        (
            1,
            "preview 2\ncut\n\nif program = 1\nend",
            RejectionCode::InvalidCommand,
            failed(ScriptFailureKind::Syntax, 4, 12),
        ),
        (
            2,
            "preview 2\nlet x = missing + 1",
            RejectionCode::InvalidCommand,
            failed(ScriptFailureKind::Runtime, 2, 9),
        ),
        (
            3,
            "preview 2\nwhile true\nend",
            RejectionCode::ResourceExhausted,
            failed(ScriptFailureKind::InstructionBudget, 2, 1),
        ),
        (
            4,
            "repeat 65\n  mute 1\nend",
            RejectionCode::ResourceExhausted,
            failed(ScriptFailureKind::ActionBudget, 2, 3),
        ),
        // Every action before the refused one is discarded too.
        (
            5,
            "preview 2\nmute 1\n  cut 9",
            RejectionCode::NotFound,
            failed(ScriptFailureKind::Refused, 3, 3),
        ),
        // The show's own refusals apply: one take of Program per frame.
        (
            6,
            "cut 2\ncut 3",
            RejectionCode::Conflict,
            failed(ScriptFailureKind::Refused, 2, 1),
        ),
        (
            7,
            "overlay 9, 2",
            RejectionCode::InvalidCommand,
            failed(ScriptFailureKind::Refused, 1, 1),
        ),
        (
            8,
            "title_text 2, 1, \"x\"",
            RejectionCode::InvalidCommand,
            failed(ScriptFailureKind::Refused, 1, 1),
        ),
    ] {
        accepted(&submit(
            &mut engine,
            &format!("set-{script}"),
            set(script, source),
        ));
        let expected = engine.show().clone();
        let (found_code, found_reason, message) =
            refusal(&submit(&mut engine, &format!("run-{script}"), run(script)));
        assert_eq!(
            (found_code, found_reason),
            (code, Some(reason)),
            "{source}: {message}"
        );
        assert!(
            message.starts_with(&format!("script {script} stopped at line ")),
            "{message}"
        );
        assert_eq!(engine.show(), &expected, "{source}");
    }
    assert_eq!(engine.show().desired_switcher(), before.desired_switcher());
    assert_eq!(
        refusal(&submit(&mut engine, "run-unknown", run(99))).1,
        Some(RefusalReason::UnknownId {
            kind: IdKind::Script,
            id: 99,
        })
    );
}

#[test]
fn script_edits_are_undoable_and_padlocked_and_runs_are_live() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "cut 2")));
    accepted(&submit(&mut engine, "set-2", set(1, "cut 3")));
    assert_eq!(engine.show().script(1).unwrap().source, "cut 3");
    accepted(&submit(
        &mut engine,
        "undo-1",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().script(1).unwrap().source, "cut 2");
    accepted(&submit(
        &mut engine,
        "undo-2",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert!(engine.show().script(1).is_none());
    accepted(&submit(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().script(1).unwrap().source, "cut 2");

    // A run is a live operation: it leaves the undo history alone.
    let depth = engine.undo_entries().len();
    accepted(&submit(&mut engine, "run", run(1)));
    assert_eq!(engine.undo_entries().len(), depth);

    // Removal, bounds, and the padlock.
    assert_eq!(
        refusal(&submit(
            &mut engine,
            "remove-unknown",
            EngineCommand::SetScript {
                script: 5,
                definition: None
            }
        ))
        .1,
        Some(RefusalReason::UnknownId {
            kind: IdKind::Script,
            id: 5,
        })
    );
    let blank = EngineCommand::SetScript {
        script: 2,
        definition: Some(EngineScriptDefinition {
            name: " ".to_owned(),
            source: String::new(),
        }),
    };
    assert!(matches!(
        refusal(&submit(&mut engine, "blank", blank)).1,
        Some(RefusalReason::InvalidName { .. })
    ));
    for script in 2..=16 {
        accepted(&submit(
            &mut engine,
            &format!("fill-{script}"),
            set(script, ""),
        ));
    }
    assert!(matches!(
        refusal(&submit(&mut engine, "full", set(17, ""))).1,
        Some(RefusalReason::BoundsExceeded { .. })
    ));
    accepted(&submit(
        &mut engine,
        "remove",
        EngineCommand::SetScript {
            script: 16,
            definition: None,
        },
    ));
    assert!(engine.show().script(16).is_none());
    accepted(&submit(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    assert_eq!(
        refusal(&submit(&mut engine, "locked", set(1, "cut 4"))).1,
        Some(RefusalReason::ProjectLocked)
    );
    // The padlock guards edits, not the show: a script still runs.
    tick(&mut engine, 2);
    accepted(&submit(&mut engine, "run-locked", run(1)));
}

#[test]
fn a_run_replays_from_its_receipt() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set", set(1, "cut 2")));
    let first = submit(&mut engine, "run", run(1));
    let again = submit(&mut engine, "run", run(1));
    assert!(again.replayed);
    assert_eq!(first.receipt, again.receipt);
}

/// Ticks `frames` frames as the daemon cue clock does: before each frame,
/// the advance due on that frame (a cue list's, or list 0 for the waiting
/// script run) is submitted. Returns the frames and every advance outcome.
fn drive(engine: &mut Engine, frames: u64) -> (Vec<FrameResult>, Vec<EngineCommandOutcome>) {
    let mut advances = Vec::new();
    let frames = (0..frames)
        .map(|_| {
            let cursor = engine.frame_cursor().get();
            if let Some((list, due)) = engine.cue_advance_frame()
                && due == cursor
            {
                let outcome = submit(
                    engine,
                    &format!("advance-{cursor}"),
                    EngineCommand::CueAdvance {
                        list,
                        frame: cursor,
                    },
                );
                assert_eq!(accepted(&outcome), cursor);
                advances.push(outcome);
            }
            engine.tick().unwrap()
        })
        .collect();
    (frames, advances)
}

/// RC-011 waits: a run's parts after a `wait` run on their due frame (the
/// cue clock's list 0), reading the show as it is then, and the run's
/// state is the show's.
#[test]
fn a_waiting_run_resumes_on_its_due_frame_and_reads_the_show_then() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(
            1,
            "preview 2\n\
             let n = 0\n\
             repeat 2\n\
             \x20 wait 5\n\
             \x20 let n = n + 1\n\
             end\n\
             if n == 2\n\
             \x20 cut preview\n\
             end\n",
        ),
    ));
    let start = accepted(&submit(&mut engine, "run", run(1)));
    let slot = engine.show().script_run().clone();
    let waiting = slot.running.as_ref().expect("the run waits");
    assert_eq!(
        (waiting.script, waiting.started_frame, waiting.due_frame),
        (1, start, start + 5)
    );
    assert_eq!(slot.fault, None);
    assert_eq!(engine.cue_advance_frame(), Some((0, start + 5)));
    // The first part's action applied on its frame.
    assert_eq!(engine.show().desired_switcher().preview(), input(12));

    // The operator changes Preview while the run waits: the last part
    // takes what Preview holds when it runs.
    tick(&mut engine, 2);
    accepted(&submit(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(13)),
    ));
    let (frames, advances) = drive(&mut engine, 14);
    assert_eq!(advances.len(), 2);
    let parts: Vec<_> = advances
        .iter()
        .flat_map(|outcome| &outcome.events)
        .filter_map(|event| match &event.payload {
            EngineEvent::ScriptRan {
                script,
                frame,
                actions,
                ..
            } => Some((*script, *frame, *actions)),
            _ => None,
        })
        .collect();
    assert_eq!(parts, vec![(1, start + 5, 1), (1, start + 10, 2)]);
    assert_eq!(program_changes(&frames), vec![(start + 10, input(13))]);
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState::default()
    );
    assert_eq!(engine.cue_advance_frame(), None);
}

#[test]
fn one_run_waits_at_a_time_and_its_script_is_frozen_until_it_stops() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "wait 50\ncut 2")));
    accepted(&submit(&mut engine, "set-2", set(2, "cut 3")));
    accepted(&submit(&mut engine, "run", run(1)));
    let busy = Some(RefusalReason::ScriptRunning { script: 1 });
    for (key, command) in [
        ("run-again", run(1)),
        ("run-other", run(2)),
        ("edit", set(1, "cut 2")),
        (
            "remove",
            EngineCommand::SetScript {
                script: 1,
                definition: None,
            },
        ),
    ] {
        let (code, reason, _) = refusal(&submit(&mut engine, key, command));
        assert_eq!(
            (code, reason),
            (RejectionCode::Conflict, busy.clone()),
            "{key}"
        );
    }
    // Another script may be edited, and an undo of the running script's
    // edit is refused like the edit.
    accepted(&submit(&mut engine, "edit-other", set(2, "cut 4")));

    // A stop ends the run on its frame: nothing more of it runs.
    let outcome = submit(&mut engine, "stop", EngineCommand::StopScript);
    let frame = accepted(&outcome);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ScriptRunChanged { script: 1, frame: at } if *at == frame
    )));
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState::default()
    );
    assert_eq!(engine.cue_advance_frame(), None);
    let (frames, advances) = drive(&mut engine, 60);
    assert_eq!(advances.len(), 0, "{advances:?}");
    assert_eq!(program_changes(&frames), Vec::new());
    let (code, _, message) = refusal(&submit(
        &mut engine,
        "stop-again",
        EngineCommand::StopScript,
    ));
    assert_eq!(code, RejectionCode::Conflict);
    assert!(message.contains("no script is running"), "{message}");
    // A stopped script runs and edits again; a stop is never undone.
    accepted(&submit(&mut engine, "edit-after", set(1, "cut 2")));
    accepted(&submit(&mut engine, "run-after", run(1)));
}

#[test]
fn a_part_that_fails_on_its_frame_faults_the_run_and_applies_nothing() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(1, "wait 3\nmute 3\ncut 9\n"),
    ));
    let start = accepted(&submit(&mut engine, "run", run(1)));
    let (frames, advances) = drive(&mut engine, 6);
    assert_eq!(advances.len(), 1);
    assert!(advances[0].events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ScriptRunChanged { script: 1, frame } if *frame == start + 3
    )));
    assert_eq!(program_changes(&frames), Vec::new());
    // The mute before the refused cut did not apply either.
    assert!(!engine.show().input_audio_strip(input(13)).unwrap().muted);
    let slot = engine.show().script_run();
    assert_eq!(slot.running, None);
    assert_eq!(
        slot.fault,
        Some(fm_model::ScriptFault {
            script: 1,
            frame: start + 3,
            failure: fm_script::FailureKind::Refused,
            at: fm_script::Span::new(3, 1),
        })
    );
    // The clock never retries a fault; a new run or a stop clears it.
    assert_eq!(engine.cue_advance_frame(), None);
    accepted(&submit(&mut engine, "stop", EngineCommand::StopScript));
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState::default()
    );
}

/// A fault's line and column name the source that ran, so an accepted edit
/// of the faulted script clears it (as a removal does); an edit of another
/// script leaves it, and an undo of the edit does not bring it back.
#[test]
fn an_edit_of_the_faulted_script_clears_its_fault() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set", set(1, "wait 3\ncut 9\n")));
    accepted(&submit(&mut engine, "set-2", set(2, "cut 2")));
    accepted(&submit(&mut engine, "run", run(1)));
    drive(&mut engine, 5);
    let fault = engine.show().script_run().fault.expect("the run faulted");
    assert_eq!(fault.script, 1);

    accepted(&submit(&mut engine, "edit-other", set(2, "cut 3")));
    assert_eq!(engine.show().script_run().fault, Some(fault));

    accepted(&submit(&mut engine, "edit", set(1, "wait 3\ncut 2\n")));
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState::default()
    );
    assert_eq!(engine.cue_advance_frame(), None);
    // The fault is live state, not part of the edit: an undo restores the
    // old source without it.
    accepted(&submit(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().script(1).unwrap().source, "wait 3\ncut 9\n");
    assert_eq!(engine.show().script_run().fault, None);
}

#[test]
fn waits_are_bounded_to_ten_minutes_of_frames() {
    let mut engine = engine();
    // 25 frames per second: ten minutes is 15 000 frames.
    assert_eq!(
        fm_model::script_wait_frames(FrameRate::new(25, 1).unwrap()),
        15_000
    );
    assert_eq!(
        fm_model::script_wait_frames(FrameRate::new(30_000, 1_001).unwrap()),
        17_983
    );
    // 600 × 60000/1001 = 35964.04, rounded up.
    assert_eq!(
        fm_model::script_wait_frames(FrameRate::new(60_000, 1_001).unwrap()),
        35_965
    );
    accepted(&submit(&mut engine, "set", set(1, "wait 15000\nwait 1")));
    accepted(&submit(&mut engine, "set-2", set(2, "wait 15001")));
    let (code, reason, _) = refusal(&submit(&mut engine, "run-2", run(2)));
    assert_eq!(code, RejectionCode::ResourceExhausted);
    assert_eq!(
        reason,
        Some(failed(ScriptFailureKind::WaitFramesBudget, 1, 1))
    );
    // The first wait fits; the second is refused when its part runs.
    let start = accepted(&submit(&mut engine, "run-1", run(1)));
    assert_eq!(engine.cue_advance_frame(), Some((0, start + 15_000)));
}

/// A cue's script step runs on its cue's frame, so a cue may not run a
/// script that waits, and a script a cue runs may not gain a wait.
#[test]
fn a_cue_never_runs_a_script_that_waits() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "cut 2\nwait 5")));
    accepted(&submit(&mut engine, "set-2", set(2, "cut 2")));
    let list = |script: u64| EngineCommand::SetCueList {
        list: 1,
        definition: Some(fm_engine::EngineCueListDefinition {
            name: "Show".to_owned(),
            cues: vec![fm_model::Cue {
                id: 1,
                name: "One".to_owned(),
                actions: vec![fm_model::CueAction::RunScript { script }],
                auto_follow: false,
            }],
        }),
    };
    let outcome = submit(&mut engine, "list-1", list(1));
    let rejection = &outcome.receipt.rejected().expect("refused").rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    assert_eq!(rejection.fields[0].code, "script_waits");
    accepted(&submit(&mut engine, "list-2", list(2)));
    let outcome = submit(&mut engine, "wait-2", set(2, "cut 2\nwait 1"));
    let rejection = &outcome.receipt.rejected().expect("refused").rejection;
    assert_eq!(rejection.fields[0].code, "script_waits");
    // A source that does not parse never waits.
    accepted(&submit(&mut engine, "broken-2", set(2, "wait (")));
}

/// The cue clock's list `0` is the waiting script run, so no cue list may
/// take id `0`: one that did would have its waits routed to the script run
/// and never resume.
#[test]
fn no_cue_list_takes_the_script_runs_list_zero() {
    let mut engine = engine();
    let outcome = submit(
        &mut engine,
        "list-0",
        EngineCommand::SetCueList {
            list: 0,
            definition: Some(fm_engine::EngineCueListDefinition {
                name: "Zero".to_owned(),
                cues: vec![fm_model::Cue {
                    id: 1,
                    name: "One".to_owned(),
                    actions: vec![
                        fm_model::CueAction::Wait { frames: 3 },
                        fm_model::CueAction::Preview { input: input(12) },
                    ],
                    auto_follow: false,
                }],
            }),
        },
    );
    let rejection = &outcome.receipt.rejected().expect("refused").rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    assert_eq!(
        (
            rejection.fields[0].field.as_str(),
            rejection.fields[0].code.as_str()
        ),
        ("list", "zero")
    );
    assert_eq!(engine.show().cue_lists(), []);
    // A restored show is checked the same way.
    let mut show = engine.show().clone();
    let list = fm_model::CueList {
        id: 0,
        name: "Zero".to_owned(),
        cues: Vec::new(),
        state: fm_model::CueRunState::default(),
    };
    assert!(show.set_cue_lists(vec![list]).is_err());
}

/// A saved run's due frame lies within its waited frames of the frame
/// cursor; a hand-edited one further ahead is refused on restore, so the
/// clock (and a local run that plays it to the end) is never held for
/// ever.
#[test]
fn a_restored_run_due_beyond_its_waits_is_refused() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set", set(1, "cut 2\nwait 40\ncut 3")));
    accepted(&submit(&mut engine, "run", run(1)));
    tick(&mut engine, 2);
    let snapshot = engine.snapshot().expect("idle");
    let restore = |show: ShowState| {
        Engine::restore_persisted(
            show,
            snapshot.realized_switcher().clone(),
            snapshot.frame_rate(),
            ClockDomainId::new(NonZeroU128::new(100).unwrap()),
            fm_engine::EngineRestoreState {
                state_epoch: snapshot.state_epoch(),
                revision: snapshot.revision(),
                event_sequence: snapshot.event_sequence(),
                runtime_generation: snapshot.runtime_generation(),
                clock_time: snapshot.clock_time(),
                frame_cursor: fm_scheduler::FrameNumber::new(snapshot.frames_rendered()),
                receipts: snapshot.receipts().to_vec(),
            },
        )
    };
    let restored = restore(snapshot.show().clone()).expect("the saved run restores");
    assert_eq!(restored.show().script_run(), snapshot.show().script_run());

    let mut slot = snapshot.show().script_run().clone();
    slot.running.as_mut().expect("the run waits").due_frame = u64::MAX / 2;
    let mut show = snapshot.show().clone();
    // The slot fits the script (the model checks no frame cursor) ...
    show.set_script_run(slot, snapshot.frame_rate()).unwrap();
    // ... but not the cursor it is restored at.
    assert!(matches!(
        restore(show).err(),
        Some(fm_engine::SnapshotError::ScriptRunOutOfReach { due_frame, .. })
            if due_frame == u64::MAX / 2
    ));
}
