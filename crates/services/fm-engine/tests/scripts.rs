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
