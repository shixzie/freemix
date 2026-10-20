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
        position: None,
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
                definition: None,
                position: None,
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
        position: None,
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
            position: None,
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
/// every advance due on that frame (a cue list's, or list 0 for each
/// waiting script run) is submitted. Returns the frames and every advance
/// outcome.
fn drive(engine: &mut Engine, frames: u64) -> (Vec<FrameResult>, Vec<EngineCommandOutcome>) {
    let mut advances = Vec::new();
    let frames = (0..frames)
        .map(|_| {
            let cursor = engine.frame_cursor().get();
            let mut turn = 0;
            while let Some((list, due)) = engine.cue_advance_frame()
                && due == cursor
            {
                let outcome = submit(
                    engine,
                    &format!("advance-{cursor}-{turn}"),
                    EngineCommand::CueAdvance {
                        list,
                        frame: cursor,
                    },
                );
                assert_eq!(accepted(&outcome), cursor);
                advances.push(outcome);
                turn += 1;
            }
            engine.tick().unwrap()
        })
        .collect();
    (frames, advances)
}

fn stop(run: Option<u64>) -> EngineCommand {
    EngineCommand::StopScript { run }
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
    let waiting = slot.run_of(1).expect("the run waits");
    assert_eq!(
        (
            waiting.run,
            waiting.script,
            waiting.started_frame,
            waiting.due_frame
        ),
        (1, 1, start, start + 5)
    );
    assert_eq!(slot.faults, []);
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
        fm_model::ScriptRunState {
            last_run: 1,
            ..fm_model::ScriptRunState::default()
        }
    );
    assert_eq!(engine.cue_advance_frame(), None);
}

#[test]
fn a_script_runs_once_at_a_time_and_is_frozen_until_it_stops() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "wait 50\ncut 2")));
    accepted(&submit(&mut engine, "set-2", set(2, "preview 3")));
    accepted(&submit(&mut engine, "run", run(1)));
    // Another script runs alongside (Protocol 2.69).
    accepted(&submit(&mut engine, "run-other", run(2)));
    let busy = Some(RefusalReason::ScriptRunning { script: 1 });
    let (_, _, message) = refusal(&submit(&mut engine, "run-twice", run(1)));
    assert!(message.contains("already running (run 1)"), "{message}");
    for (key, command) in [
        ("run-again", run(1)),
        ("edit", set(1, "cut 2")),
        (
            "remove",
            EngineCommand::SetScript {
                script: 1,
                definition: None,
                position: None,
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
    let outcome = submit(&mut engine, "stop", stop(Some(1)));
    let frame = accepted(&outcome);
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ScriptRunChanged { script: 1, frame: at } if *at == frame
    )));
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState {
            last_run: 1,
            ..fm_model::ScriptRunState::default()
        }
    );
    assert_eq!(engine.cue_advance_frame(), None);
    let (frames, advances) = drive(&mut engine, 60);
    assert_eq!(advances.len(), 0, "{advances:?}");
    assert_eq!(program_changes(&frames), Vec::new());
    let (code, _, message) = refusal(&submit(&mut engine, "stop-again", stop(None)));
    assert_eq!(code, RejectionCode::Conflict);
    assert!(message.contains("no script is running"), "{message}");
    let (code, _, message) = refusal(&submit(&mut engine, "stop-1-again", stop(Some(1))));
    assert_eq!(code, RejectionCode::NotFound);
    assert!(message.contains("no script run 1"), "{message}");
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
    assert_eq!(slot.runs, []);
    assert_eq!(
        slot.fault_of(1).cloned(),
        Some(fm_model::ScriptFault {
            run: 1,
            script: 1,
            frame: start + 3,
            failure: fm_script::FailureKind::Refused,
            at: fm_script::Span::new(3, 1),
            // The refusal had no typed reason; its message is kept.
            detail: fm_command::FaultDetail {
                reason: None,
                message: "the show has no input number 9".to_owned(),
            },
        })
    );
    // The clock never retries a fault; a new run or a stop clears it.
    assert_eq!(engine.cue_advance_frame(), None);
    accepted(&submit(&mut engine, "stop", stop(Some(1))));
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState {
            last_run: 1,
            ..fm_model::ScriptRunState::default()
        }
    );
}

/// Protocol 2.63: a deferred part refused by the show keeps the refused
/// action's typed reason and its message, exactly what a refused first
/// part says; a part that fails without a refused action keeps the
/// failure's own message.
#[test]
fn a_deferred_fault_carries_the_refused_actions_reason() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set", set(1, "wait 3\ntitle_off 2\n")));
    // The same action refused on the first frame, for comparison.
    accepted(&submit(&mut engine, "set-2", set(2, "title_off 2\n")));
    let (_, reason, message) = refusal(&submit(&mut engine, "run-2", run(2)));
    assert_eq!(
        reason,
        Some(RefusalReason::ScriptFailed {
            failure: fm_command::ScriptFailureKind::Refused,
            line: 1,
            column: 1,
        })
    );
    assert!(
        message.ends_with("the script names input 12, which is not a title"),
        "{message}"
    );

    accepted(&submit(&mut engine, "run", run(1)));
    drive(&mut engine, 5);
    let fault = engine
        .show()
        .script_run()
        .fault_of(1)
        .cloned()
        .expect("the run faulted");
    assert_eq!(
        (fault.failure, fault.at),
        (fm_script::FailureKind::Refused, fm_script::Span::new(2, 1))
    );
    assert_eq!(
        fault.detail,
        fm_command::FaultDetail {
            reason: Some(RefusalReason::UnknownId {
                kind: fm_command::IdKind::Title,
                id: 12,
            }),
            message: "the script names input 12, which is not a title".to_owned(),
        }
    );

    // A runtime error refuses no action: the failure's message is kept.
    accepted(&submit(&mut engine, "stop", stop(None)));
    accepted(&submit(&mut engine, "set-3", set(1, "wait 3\ncut 1 / 0\n")));
    accepted(&submit(&mut engine, "run-3", run(1)));
    drive(&mut engine, 5);
    let fault = engine
        .show()
        .script_run()
        .fault_of(1)
        .cloned()
        .expect("the run faulted");
    assert_eq!(fault.failure, fm_script::FailureKind::Runtime);
    assert_eq!(fault.detail.reason, None);
    assert!(
        fm_command::FaultDetail::message_is_valid(&fault.detail.message),
        "{:?}",
        fault.detail
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
    let fault = engine
        .show()
        .script_run()
        .fault_of(1)
        .cloned()
        .expect("the run faulted");
    assert_eq!(fault.script, 1);

    accepted(&submit(&mut engine, "edit-other", set(2, "cut 3")));
    assert_eq!(engine.show().script_run().fault_of(1), Some(&fault));

    accepted(&submit(&mut engine, "edit", set(1, "wait 3\ncut 2\n")));
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState {
            last_run: 1,
            ..fm_model::ScriptRunState::default()
        }
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
    assert_eq!(engine.show().script_run().fault_of(1), None);
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
        position: None,
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
                        fm_model::CueAction::Preview {
                            input: fm_types::InputTarget::Fixed(input(12)),
                        },
                    ],
                    auto_follow: false,
                }],
            }),
            position: None,
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
    slot.runs.first_mut().expect("the run waits").due_frame = u64::MAX / 2;
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

/// A data update's value out of range is refused naming what the bound is
/// for: a value to set as well as a step.
#[test]
fn an_out_of_range_title_number_is_refused_for_what_it_is() {
    let mut engine = engine();
    for (script, source) in [
        (1, "title_number 1, 1, 1000000000000"),
        (2, "title_add 1, 1, -1000000000000"),
        (3, "title_add 1, 1, 0"),
    ] {
        accepted(&submit(
            &mut engine,
            &format!("set-{script}"),
            set(script, source),
        ));
        let (code, _, message) =
            refusal(&submit(&mut engine, &format!("run-{script}"), run(script)));
        assert_eq!(code, RejectionCode::InvalidCommand, "{source}: {message}");
        assert!(
            message.contains(
                "a title number of at most 999999999999 either way (a step of it nonzero)"
            ),
            "{source}: {message}"
        );
    }
}

/// The `(script, frame)` of every part an advance ran.
fn parts_ran(advances: &[EngineCommandOutcome]) -> Vec<(u64, u64)> {
    advances
        .iter()
        .flat_map(|outcome| &outcome.events)
        .filter_map(|event| match &event.payload {
            EngineEvent::ScriptRan { script, frame, .. } => Some((*script, *frame)),
            _ => None,
        })
        .collect()
}

fn muted(engine: &Engine, value: u128) -> bool {
    engine.show().input_audio_strip(input(value)).unwrap().muted
}

/// Protocol 2.69: runs of different scripts wait together, each with its
/// own id, and each part runs on its own due frame, interleaved.
#[test]
fn runs_of_different_scripts_wait_together_and_interleave() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set-1",
        set(1, "wait 3\npreview 2\nwait 4\npreview 3\n"),
    ));
    accepted(&submit(
        &mut engine,
        "set-2",
        set(2, "wait 2\nmute 1\nwait 3\nmute 2\n"),
    ));
    let start = accepted(&submit(&mut engine, "run-1", run(1)));
    assert_eq!(accepted(&submit(&mut engine, "run-2", run(2))), start);
    let runs = engine.show().script_run().clone();
    assert_eq!(
        runs.runs
            .iter()
            .map(|run| (run.run, run.script, run.due_frame))
            .collect::<Vec<_>>(),
        vec![(1, 1, start + 3), (2, 2, start + 2)]
    );
    assert_eq!(runs.last_run, 2);
    // The earliest due goes first, whatever its id.
    assert_eq!(engine.cue_advance_frame(), Some((0, start + 2)));

    let (_, advances) = drive(&mut engine, 10);
    assert_eq!(
        parts_ran(&advances),
        vec![
            (2, start + 2),
            (1, start + 3),
            (2, start + 5),
            (1, start + 7)
        ]
    );
    assert_eq!(engine.show().desired_switcher().preview(), input(13));
    assert!(muted(&engine, 11) && muted(&engine, 12));
    assert_eq!(engine.show().script_run().runs, []);
    assert_eq!(engine.cue_advance_frame(), None);
    // Ids are never reused.
    accepted(&submit(&mut engine, "run-1-again", run(1)));
    assert_eq!(engine.show().script_run().run_of(1).unwrap().run, 3);
}

/// Runs due on the same frame resume on it together, also when the first
/// advance is accepted ahead of that frame and holds it.
#[test]
fn runs_due_on_the_same_frame_resume_together() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "wait 5\npreview 2\n")));
    accepted(&submit(&mut engine, "set-2", set(2, "wait 5\nmute 1\n")));
    let start = accepted(&submit(&mut engine, "run-1", run(1)));
    accepted(&submit(&mut engine, "run-2", run(2)));
    let due = start + 5;
    assert_eq!(engine.cue_advance_frame(), Some((0, due)));
    let advance = || EngineCommand::CueAdvance {
        list: 0,
        frame: due,
    };
    // Both advances are accepted ahead, for the same frame.
    assert_eq!(accepted(&submit(&mut engine, "advance-1", advance())), due);
    assert_eq!(engine.cue_advance_frame(), Some((0, due)));
    assert_eq!(accepted(&submit(&mut engine, "advance-2", advance())), due);
    assert_eq!(engine.cue_advance_frame(), None);
    // Any other command waits for the held frame.
    let (_, reason, _) = refusal(&submit(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(13)),
    ));
    assert_eq!(reason, Some(RefusalReason::CueScheduled { frame: due }));
    tick(&mut engine, 7);
    assert_eq!(engine.show().desired_switcher().preview(), input(12));
    assert!(muted(&engine, 11));
    assert_eq!(engine.show().script_run().runs, []);
}

/// At most `MAX_SCRIPT_RUNS` runs wait; a run that does not wait is never
/// bounded by them.
#[test]
fn the_waiting_runs_are_bounded() {
    let mut engine = engine();
    let bound = fm_model::MAX_SCRIPT_RUNS as u64;
    for script in 1..=bound + 2 {
        let source = if script == bound + 2 {
            "preview 2"
        } else {
            "wait 100\ncut 2"
        };
        accepted(&submit(
            &mut engine,
            &format!("set-{script}"),
            set(script, source),
        ));
    }
    for script in 1..=bound {
        accepted(&submit(&mut engine, &format!("run-{script}"), run(script)));
    }
    let (code, reason, message) = refusal(&submit(&mut engine, "run-over", run(bound + 1)));
    assert_eq!(code, RejectionCode::ResourceExhausted);
    assert_eq!(reason, Some(RefusalReason::bounds("script_runs", bound)));
    assert!(message.contains("stop one first"), "{message}");
    assert_eq!(engine.show().script_run().runs.len() as u64, bound);
    accepted(&submit(&mut engine, "run-quick", run(bound + 2)));
    // A stop makes room.
    accepted(&submit(&mut engine, "stop-3", stop(Some(3))));
    accepted(&submit(&mut engine, "run-over-again", run(bound + 1)));
    assert_eq!(
        engine.show().script_run().run_of(bound + 1).unwrap().run,
        bound + 1
    );
}

/// A part that fails faults its own run only; a stop ends one run or
/// clears one fault, and a stop of every run ends them all at once.
#[test]
fn a_fault_or_stop_touches_only_its_own_run_until_all_are_stopped() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "wait 2\ncut 9\n")));
    accepted(&submit(&mut engine, "set-2", set(2, "wait 4\npreview 2\n")));
    accepted(&submit(
        &mut engine,
        "set-3",
        set(3, "wait 50\npreview 3\n"),
    ));
    accepted(&submit(&mut engine, "set-4", set(4, "wait 60\nmute 1\n")));
    for script in 1..=4 {
        accepted(&submit(&mut engine, &format!("run-{script}"), run(script)));
    }
    drive(&mut engine, 6);
    let runs = engine.show().script_run().clone();
    assert_eq!(runs.fault_of(1).map(|fault| fault.run), Some(1));
    assert_eq!(engine.show().desired_switcher().preview(), input(12));
    assert_eq!(
        runs.runs.iter().map(|run| run.run).collect::<Vec<_>>(),
        vec![3, 4]
    );

    // Stop run 3: run 4 still waits and the fault stays.
    let outcome = submit(&mut engine, "stop-3", stop(Some(3)));
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::ScriptRunChanged { script: 3, .. }
    )));
    let runs = engine.show().script_run().clone();
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(runs.run_of(4).map(|run| run.run), Some(4));
    assert!(runs.fault_of(1).is_some());
    // Clearing run 1's fault names its run too.
    accepted(&submit(&mut engine, "clear-1", stop(Some(1))));
    assert_eq!(engine.show().script_run().faults, []);

    accepted(&submit(&mut engine, "rerun-3", run(3)));
    accepted(&submit(&mut engine, "rerun-1", run(1)));
    drive(&mut engine, 3);
    let outcome = submit(&mut engine, "stop-all", stop(None));
    let frame = accepted(&outcome);
    assert!(
        outcome.events.iter().any(|event| matches!(
            &event.payload,
            EngineEvent::ScriptRunsStopped { scripts, frame: at }
                if *scripts == vec![1, 3, 4] && *at == frame
        )),
        "{:?}",
        outcome.events
    );
    assert_eq!(
        *engine.show().script_run(),
        fm_model::ScriptRunState {
            last_run: 6,
            ..fm_model::ScriptRunState::default()
        }
    );
    let (frames, advances) = drive(&mut engine, 70);
    assert_eq!(advances.len(), 0);
    assert_eq!(program_changes(&frames), Vec::new());
    assert!(!muted(&engine, 11));
}

/// Two interleaved runs saved mid-wait and restored resume exactly as the
/// show that kept running: the same parts on the same frames, the same
/// ids, the same show.
#[test]
fn two_interleaved_runs_restore_and_replay_deterministically() {
    let build = || {
        let mut engine = engine();
        accepted(&submit(
            &mut engine,
            "set-1",
            set(
                1,
                "let n = 0\nrepeat 3\n  wait 3\n  let n = n + 1\n  mute n\nend\n",
            ),
        ));
        accepted(&submit(
            &mut engine,
            "set-2",
            set(
                2,
                "repeat 2\n  wait 4\n  if program == 1\n    cut 2\n  else\n    cut 1\n  end\nend\n",
            ),
        ));
        accepted(&submit(&mut engine, "run-1", run(1)));
        tick(&mut engine, 1);
        accepted(&submit(&mut engine, "run-2", run(2)));
        let (_, advances) = drive(&mut engine, 5);
        (engine, parts_ran(&advances))
    };
    let (mut original, before) = build();
    let (twin, twin_before) = build();
    assert_eq!(before, twin_before);
    assert_eq!(original.show(), twin.show());
    assert_eq!(
        original.show().script_run().runs.len(),
        2,
        "{:?}",
        original.show().script_run()
    );

    let snapshot = original.snapshot().expect("idle");
    let mut restored = Engine::restore_persisted(
        snapshot.show().clone(),
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
    .expect("both runs restore");
    let (original_frames, original_advances) = drive(&mut original, 12);
    let (restored_frames, restored_advances) = drive(&mut restored, 12);
    let after = parts_ran(&original_advances);
    assert_eq!(after, parts_ran(&restored_advances));
    // Both scripts ran after the save, interleaved.
    assert!(after.iter().any(|(script, _)| *script == 1));
    assert!(after.iter().any(|(script, _)| *script == 2));
    assert_eq!(
        program_changes(&original_frames),
        program_changes(&restored_frames)
    );
    assert_eq!(original.show(), restored.show());
    assert_eq!(original.show().script_run().runs, []);
    assert_eq!(original.show().script_run().last_run, 2);
}

/// A restored show whose last issued run id is the largest one (a
/// hand-edited project: validation accepts it) refuses a run that would
/// wait instead of issuing that id again, which left two runs sharing it
/// and a show no project could save. A run that does not wait needs no id.
#[test]
fn a_run_never_reuses_the_last_run_id() {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "wait 50\ncut 2")));
    accepted(&submit(&mut engine, "set-2", set(2, "wait 50\ncut 3")));
    accepted(&submit(&mut engine, "set-3", set(3, "preview 2")));
    accepted(&submit(&mut engine, "run-1", run(1)));
    let mut show = engine.show().clone();
    let mut runs = show.script_run().clone();
    runs.runs[0].run = u64::MAX;
    runs.last_run = u64::MAX;
    show.set_script_run(runs, rate).unwrap();
    let mut engine = Engine::new(
        show,
        rate,
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    );
    let before = engine.show().script_run().clone();
    let (code, reason, _) = refusal(&submit(&mut engine, "run-2", run(2)));
    assert_eq!(code, RejectionCode::ResourceExhausted);
    assert_eq!(reason, Some(RefusalReason::bounds("last_run", u64::MAX)));
    assert_eq!(*engine.show().script_run(), before);
    fm_model::validate_script_run(engine.show().script_run(), engine.show().scripts(), rate)
        .expect("the runs stay valid");
    accepted(&submit(&mut engine, "run-3", run(3)));
    assert_eq!(engine.show().desired_switcher().preview(), input(12));
}

/// Two runs due on the frame a cue list's waiting cue is due on: the cue
/// goes first, holding the frame when its advance is accepted ahead, and
/// both runs join it there, the lowest run id first.
#[test]
fn runs_due_with_a_cue_join_its_held_frame() {
    let mut engine = engine();
    accepted(&submit(&mut engine, "set-1", set(1, "wait 5\npreview 2\n")));
    accepted(&submit(&mut engine, "set-2", set(2, "wait 5\nmute 1\n")));
    accepted(&submit(
        &mut engine,
        "list",
        EngineCommand::SetCueList {
            list: 1,
            definition: Some(fm_engine::EngineCueListDefinition {
                name: "Show".to_owned(),
                cues: vec![fm_model::Cue {
                    id: 1,
                    name: "One".to_owned(),
                    actions: vec![
                        fm_model::CueAction::Wait { frames: 5 },
                        fm_model::CueAction::AudioMute {
                            input: fm_types::InputTarget::Fixed(input(12)),
                            muted: true,
                        },
                    ],
                    auto_follow: false,
                }],
            }),
            position: None,
        },
    ));
    let start = accepted(&submit(
        &mut engine,
        "go",
        EngineCommand::CueControl {
            list: 1,
            operation: fm_engine::EngineCueOperation::Go {
                cue: Some(1),
                targets: fm_types::TargetBindings::default(),
            },
        },
    ));
    assert_eq!(accepted(&submit(&mut engine, "run-1", run(1))), start);
    assert_eq!(accepted(&submit(&mut engine, "run-2", run(2))), start);
    let due = start + 5;
    let mut order = Vec::new();
    while let Some((list, at)) = engine.cue_advance_frame() {
        assert_eq!(at, due);
        let outcome = submit(
            &mut engine,
            &format!("advance-{}", order.len()),
            EngineCommand::CueAdvance { list, frame: at },
        );
        assert_eq!(accepted(&outcome), due);
        order.push((list, parts_ran(std::slice::from_ref(&outcome))));
    }
    assert_eq!(
        order,
        vec![(1, vec![]), (0, vec![(1, due)]), (0, vec![(2, due)])]
    );
    tick(&mut engine, 7);
    assert!(muted(&engine, 11) && muted(&engine, 12));
    assert_eq!(engine.show().desired_switcher().preview(), input(12));
}
