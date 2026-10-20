//! Undoing the removal of a shortcut (RC-004), script (RC-011), or cue list
//! (RC-017) puts it back at its old place in the project's list, not at the
//! end; redo and undo again agree; and the edit an undo resolves to (what
//! the authority journals) carries that place, so replaying it on a
//! restored engine reaches the same order.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey};
use fm_engine::{
    Engine, EngineCommand, EngineCommandOutcome, EngineCueListDefinition, EngineScriptDefinition,
    ShowState,
};
use fm_model::{Cue, CueAction, Shortcut, ShortcutAction, TransitionEffect};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn engine() -> Engine {
    Engine::new(
        ShowState::new(
            "list order show",
            [11, 12, 13]
                .into_iter()
                .enumerate()
                .map(|(index, value)| (input(value), format!("Camera {}", index + 1)))
                .collect(),
            input(11),
            input(12),
        )
        .unwrap(),
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn submit(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap()
}

fn accept(engine: &mut Engine, key: &str, command: EngineCommand) {
    let outcome = submit(engine, key, command);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key} was rejected: {:?}",
        outcome.receipt
    );
}

fn undo() -> EngineCommand {
    EngineCommand::Undo {
        expected_edit: None,
    }
}

fn redo() -> EngineCommand {
    EngineCommand::Redo {
        expected_edit: None,
    }
}

fn shortcut(id: u64) -> EngineCommand {
    EngineCommand::SetShortcut {
        shortcut: id,
        definition: Some(Box::new(Shortcut {
            id,
            name: format!("Shortcut {id}"),
            enabled: true,
            key: None,
            osc: None,
            action: ShortcutAction::Actions(vec![CueAction::Preview { input: input(13) }]),
        })),
        position: None,
    }
}

fn script(id: u64) -> EngineCommand {
    EngineCommand::SetScript {
        script: id,
        definition: Some(EngineScriptDefinition {
            name: format!("Script {id}"),
            source: "preview 13\n".to_owned(),
        }),
        position: None,
    }
}

fn cue_list(id: u64) -> EngineCommand {
    EngineCommand::SetCueList {
        list: id,
        definition: Some(EngineCueListDefinition {
            name: format!("List {id}"),
            cues: vec![Cue {
                id: 1,
                name: "Cut".to_owned(),
                auto_follow: false,
                actions: vec![CueAction::Take {
                    input: Some(input(13)),
                    effect: TransitionEffect::Cut,
                    duration_frames: 0,
                }],
            }],
        }),
        position: None,
    }
}

/// One kind of authored project list.
struct Kind {
    set: fn(u64) -> EngineCommand,
    remove: fn(u64) -> EngineCommand,
    order: fn(&Engine) -> Vec<u64>,
}

const SHORTCUTS: Kind = Kind {
    set: shortcut,
    remove: |id| EngineCommand::SetShortcut {
        shortcut: id,
        definition: None,
        position: None,
    },
    order: |engine| engine.show().shortcuts().iter().map(|s| s.id).collect(),
};

const SCRIPTS: Kind = Kind {
    set: script,
    remove: |id| EngineCommand::SetScript {
        script: id,
        definition: None,
        position: None,
    },
    order: |engine| engine.show().scripts().iter().map(|s| s.id).collect(),
};

const CUE_LISTS: Kind = Kind {
    set: cue_list,
    remove: |id| EngineCommand::SetCueList {
        list: id,
        definition: None,
        position: None,
    },
    order: |engine| engine.show().cue_lists().iter().map(|l| l.id).collect(),
};

/// Builds lists 1, 2, 3 and removes 2 on a fresh engine.
fn removed_middle(kind: &Kind) -> Engine {
    let mut engine = engine();
    for id in [1, 2, 3] {
        accept(&mut engine, &format!("set-{id}"), (kind.set)(id));
    }
    accept(&mut engine, "remove-2", (kind.remove)(2));
    assert_eq!((kind.order)(&engine), [1, 3]);
    engine
}

fn assert_undo_restores_the_old_place(kind: &Kind) {
    let mut engine = removed_middle(kind);
    // The edit the undo resolves to, which the authority journals.
    let journaled = engine
        .undo_entries()
        .last()
        .expect("the removal is undoable")
        .inverse()
        .clone();
    accept(&mut engine, "undo-remove", undo());
    assert_eq!((kind.order)(&engine), [1, 2, 3]);
    accept(&mut engine, "redo-remove", redo());
    assert_eq!((kind.order)(&engine), [1, 3]);
    accept(&mut engine, "undo-again", undo());
    assert_eq!((kind.order)(&engine), [1, 2, 3]);

    // A journal replay on a restored engine reaches the same order.
    let mut replayed = removed_middle(kind);
    accept(&mut replayed, "undo-remove", journaled);
    assert_eq!((kind.order)(&replayed), [1, 2, 3]);
    assert_eq!(replayed.show(), engine.show());

    // An edit keeps its place, and its undo too.
    let mut edited = removed_middle(kind);
    accept(&mut edited, "edit-1", (kind.set)(1));
    accept(&mut edited, "undo-edit", undo());
    assert_eq!((kind.order)(&edited), [1, 3]);
    // An add undone and redone returns to the end.
    accept(&mut edited, "add-4", (kind.set)(4));
    accept(&mut edited, "undo-add", undo());
    accept(&mut edited, "redo-add", redo());
    assert_eq!((kind.order)(&edited), [1, 3, 4]);
    accept(&mut edited, "undo-add-again", undo());
    assert_eq!((kind.order)(&edited), [1, 3]);
}

#[test]
fn undoing_a_shortcut_removal_restores_it_at_its_old_place() {
    assert_undo_restores_the_old_place(&SHORTCUTS);
}

#[test]
fn undoing_a_script_removal_restores_it_at_its_old_place() {
    assert_undo_restores_the_old_place(&SCRIPTS);
}

#[test]
fn undoing_a_cue_list_removal_restores_it_at_its_old_place() {
    assert_undo_restores_the_old_place(&CUE_LISTS);
}

/// An explicit position places a new entry, or moves an existing one,
/// clamped to the end of the list.
#[test]
fn a_position_places_or_moves_an_entry() {
    let mut engine = engine();
    for id in [1, 2, 3] {
        accept(&mut engine, &format!("set-{id}"), shortcut(id));
    }
    let at = |id: u64, position: usize| match shortcut(id) {
        EngineCommand::SetShortcut {
            shortcut,
            definition,
            ..
        } => EngineCommand::SetShortcut {
            shortcut,
            definition,
            position: Some(position),
        },
        _ => unreachable!(),
    };
    accept(&mut engine, "insert-4", at(4, 0));
    assert_eq!((SHORTCUTS.order)(&engine), [4, 1, 2, 3]);
    accept(&mut engine, "move-1", at(1, 9));
    assert_eq!((SHORTCUTS.order)(&engine), [4, 2, 3, 1]);
    accept(&mut engine, "undo-move", undo());
    assert_eq!((SHORTCUTS.order)(&engine), [4, 1, 2, 3]);
}
