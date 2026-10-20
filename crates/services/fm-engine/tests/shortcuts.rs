//! User shortcuts (RC-004): edits are undoable and padlocked project edits;
//! a fire of cue actions runs them on one frame, all or nothing; a fire of
//! a cue operation or script run is that command; a disabled or unknown
//! shortcut is refused.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdKind, IdempotencyKey, RefusalReason, RejectionCode};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandOutcome, EngineCueListDefinition,
    EngineCueOperation, EngineEvent, EngineScriptDefinition, ShowState,
};
use fm_model::{
    Cue, CueAction, Shortcut, ShortcutAction, ShortcutCueOperation, ShortcutKey, TransitionEffect,
};
use fm_switcher::OverlayChannelId;
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "shortcut show",
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

fn shortcut(id: u64, action: ShortcutAction) -> Shortcut {
    Shortcut {
        id,
        name: format!("Shortcut {id}"),
        enabled: true,
        key: Some(ShortcutKey::plain(format!("F{}", id + 8))),
        osc: Some(format!("s{id}")),
        action,
    }
}

fn set(definition: Shortcut) -> EngineCommand {
    EngineCommand::SetShortcut {
        shortcut: definition.id,
        definition: Some(Box::new(definition)),
        position: None,
    }
}

fn fire(shortcut: u64) -> EngineCommand {
    EngineCommand::FireShortcut { shortcut }
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

fn actions(actions: Vec<CueAction>) -> ShortcutAction {
    ShortcutAction::Actions(actions)
}

#[test]
fn a_fire_runs_every_action_on_one_frame() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(shortcut(
            1,
            actions(vec![
                CueAction::Preview { input: input(12) },
                CueAction::OverlayOn {
                    channel: 2,
                    input: input(13),
                },
                CueAction::AudioMute {
                    input: input(14),
                    muted: true,
                },
            ]),
        )),
    ));
    let outcome = submit(&mut engine, "fire", fire(1));
    let frame = accepted(&outcome);
    let fired = outcome
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EngineEvent::ShortcutFired {
                shortcut,
                frame,
                actions,
                ..
            } => Some((*shortcut, *frame, *actions)),
            _ => None,
        })
        .expect("a fire reports what it ran");
    assert_eq!(fired, (1, frame, 3));
    let switcher = engine.show().desired_switcher();
    assert_eq!(switcher.preview(), input(12));
    assert_eq!(
        switcher.overlay(OverlayChannelId::new(2).unwrap()).source(),
        Some(input(13))
    );
    assert!(engine.show().input_audio_strip(input(14)).unwrap().muted);
    // A fire is a live operation: nothing to undo but the edit itself.
    assert_eq!(engine.show().shortcuts().len(), 1);
}

#[test]
fn one_refused_action_refuses_the_whole_fire() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "set",
        set(shortcut(
            1,
            actions(vec![
                CueAction::Preview { input: input(12) },
                // Input 99 is not in the show.
                CueAction::OverlayOn {
                    channel: 1,
                    input: input(99),
                },
            ]),
        )),
    ));
    let (code, reason, message) = refusal(&submit(&mut engine, "fire", fire(1)));
    assert_eq!(code, RejectionCode::NotFound);
    assert!(matches!(
        reason,
        Some(RefusalReason::UnknownId {
            kind: IdKind::Input,
            ..
        })
    ));
    assert!(message.contains("shortcut 1"), "{message}");
    // Nothing of the fire applied.
    assert_eq!(engine.show().desired_switcher().preview(), input(14));
}

#[test]
fn a_disabled_or_unknown_shortcut_is_refused() {
    let mut engine = engine();
    let mut disabled = shortcut(1, actions(vec![CueAction::Preview { input: input(12) }]));
    disabled.enabled = false;
    accepted(&submit(&mut engine, "set", set(disabled)));
    let (code, _, message) = refusal(&submit(&mut engine, "fire", fire(1)));
    assert_eq!(code, RejectionCode::Conflict);
    assert!(message.contains("disabled"), "{message}");
    assert_eq!(engine.show().desired_switcher().preview(), input(14));

    let (code, reason, _) = refusal(&submit(&mut engine, "unknown", fire(7)));
    assert_eq!(code, RejectionCode::NotFound);
    assert_eq!(
        reason,
        Some(RefusalReason::UnknownId {
            kind: IdKind::Shortcut,
            id: 7
        })
    );
}

#[test]
fn a_cue_or_script_shortcut_fires_as_that_command() {
    let mut engine = engine();
    accepted(&submit(
        &mut engine,
        "list",
        EngineCommand::SetCueList {
            list: 3,
            definition: Some(EngineCueListDefinition {
                name: "Show".to_owned(),
                cues: vec![
                    Cue {
                        id: 1,
                        name: "Open".to_owned(),
                        actions: vec![
                            CueAction::Preview { input: input(13) },
                            CueAction::Delay { millis: 400 },
                            CueAction::Take {
                                input: None,
                                effect: TransitionEffect::Cut,
                                duration_frames: 0,
                            },
                        ],
                        auto_follow: false,
                    },
                    Cue {
                        id: 2,
                        name: "Close".to_owned(),
                        actions: vec![CueAction::Preview { input: input(12) }],
                        auto_follow: false,
                    },
                ],
            }),
            position: None,
        },
    ));
    accepted(&submit(
        &mut engine,
        "go",
        set(shortcut(
            1,
            ShortcutAction::Cue {
                list: 3,
                operation: ShortcutCueOperation::Go { cue: None },
            },
        )),
    ));
    let outcome = submit(&mut engine, "fire-go", fire(1));
    accepted(&outcome);
    // The fire is the cue GO: its event and its waiting delay.
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::CueListChanged { list: 3, fired, edit: false, .. } if fired == &vec![1]
    )));
    let list = engine.show().cue_list(3).unwrap();
    assert!(
        list.state.pending.is_some(),
        "the delay waits on the cue clock"
    );
    assert_eq!(engine.show().desired_switcher().preview(), input(13));

    accepted(&submit(
        &mut engine,
        "script",
        EngineCommand::SetScript {
            script: 5,
            definition: Some(EngineScriptDefinition {
                name: "Preview".to_owned(),
                source: "preview 2".to_owned(),
            }),
            position: None,
        },
    ));
    accepted(&submit(
        &mut engine,
        "run",
        set(shortcut(2, ShortcutAction::Script { script: 5 })),
    ));
    // The cue list holds the frame of its pending take; stop it first.
    accepted(&submit(
        &mut engine,
        "stop",
        EngineCommand::CueControl {
            list: 3,
            operation: EngineCueOperation::Stop,
        },
    ));
    let outcome = submit(&mut engine, "fire-run", fire(2));
    accepted(&outcome);
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(&event.payload, EngineEvent::ScriptRan { script: 5, .. }))
    );
    assert_eq!(engine.show().desired_switcher().preview(), input(12));
}

#[test]
fn edits_are_undoable_padlocked_and_keep_keys_unique() {
    let mut engine = engine();
    let first = shortcut(1, actions(vec![CueAction::Preview { input: input(12) }]));
    accepted(&submit(&mut engine, "set", set(first.clone())));
    let mut renamed = first.clone();
    renamed.name = "Renamed".to_owned();
    accepted(&submit(&mut engine, "rename", set(renamed.clone())));
    assert_eq!(engine.show().shortcut(1), Some(&renamed));
    accepted(&submit(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().shortcut(1), Some(&first));
    accepted(&submit(
        &mut engine,
        "redo",
        EngineCommand::Redo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().shortcut(1), Some(&renamed));

    // Another shortcut may not take the same key or OSC name.
    let mut clash = shortcut(2, actions(vec![CueAction::Preview { input: input(13) }]));
    clash.key = first.key.clone();
    let (code, _, message) = refusal(&submit(&mut engine, "clash-key", set(clash.clone())));
    assert_eq!(code, RejectionCode::Conflict);
    assert!(message.contains("share a key"), "{message}");
    clash.key = None;
    clash.osc = first.osc.clone();
    let (code, _, message) = refusal(&submit(&mut engine, "clash-osc", set(clash)));
    assert_eq!(code, RejectionCode::Conflict);
    assert!(message.contains("OSC"), "{message}");

    // A wait is refused: a timed sequence is a cue.
    let (code, _, _) = refusal(&submit(
        &mut engine,
        "wait",
        set(shortcut(
            3,
            actions(vec![
                CueAction::Preview { input: input(13) },
                CueAction::Delay { millis: 200 },
            ]),
        )),
    ));
    assert_eq!(code, RejectionCode::InvalidCommand);

    // Removal, and its undo.
    accepted(&submit(
        &mut engine,
        "remove",
        EngineCommand::SetShortcut {
            shortcut: 1,
            definition: None,
            position: None,
        },
    ));
    assert_eq!(engine.show().shortcuts(), []);
    accepted(&submit(
        &mut engine,
        "undo-remove",
        EngineCommand::Undo {
            expected_edit: None,
        },
    ));
    assert_eq!(engine.show().shortcut(1), Some(&renamed));

    accepted(&submit(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    assert_eq!(
        refusal(&submit(&mut engine, "locked", set(first))).1,
        Some(RefusalReason::ProjectLocked)
    );
    // A fire is a live operation, allowed under the padlock.
    accepted(&submit(&mut engine, "fire-locked", fire(1)));
}

/// A shortcut's cue actions run on one frame, so a script step among them
/// cannot wait: saving such a shortcut is refused like a cue list naming
/// a waiting script, and so is adding a wait to a script a shortcut runs.
#[test]
fn a_shortcut_running_a_waiting_script_is_refused_when_saved() {
    let script = |source: &str| EngineCommand::SetScript {
        script: 5,
        definition: Some(EngineScriptDefinition {
            name: "Script".to_owned(),
            source: source.to_owned(),
        }),
        position: None,
    };
    let script_waits = |outcome: &EngineCommandOutcome| {
        outcome
            .receipt
            .rejected()
            .expect("command should be rejected")
            .rejection
            .fields
            .iter()
            .any(|field| field.code == "script_waits")
    };
    let mut engine = engine();
    accepted(&submit(&mut engine, "waits", script("wait 5\ncut 2")));
    let refused = submit(
        &mut engine,
        "set",
        set(shortcut(
            1,
            actions(vec![CueAction::RunScript { script: 5 }]),
        )),
    );
    assert!(script_waits(&refused), "{:?}", refused.receipt);
    assert!(engine.show().shortcut(1).is_none());
    // A script shortcut runs the script on its own, which may wait.
    accepted(&submit(
        &mut engine,
        "set-run",
        set(shortcut(2, ShortcutAction::Script { script: 5 })),
    ));

    accepted(&submit(&mut engine, "no-wait", script("cut 2")));
    accepted(&submit(
        &mut engine,
        "set-again",
        set(shortcut(
            1,
            actions(vec![CueAction::RunScript { script: 5 }]),
        )),
    ));
    let refused = submit(&mut engine, "add-wait", script("wait 5\ncut 2"));
    assert!(script_waits(&refused), "{:?}", refused.receipt);
    assert_eq!(engine.show().script(5).unwrap().source, "cut 2");
}
