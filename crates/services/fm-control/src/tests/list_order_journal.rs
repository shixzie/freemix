//! An accepted undo is journaled as the wire payload of the edit it
//! resolves to. Undoing the removal of a shortcut, script, or cue list
//! journals its old `position` (Protocol 2.79), so the journal line,
//! decoded and replayed on a restored authority, puts it back in place
//! rather than at the end of the list.

use fm_protocol::{CueListSpec, ScriptSpec, ShortcutSpec, WireCue, WireShortcutAction};

use super::*;

/// One kind of authored project list.
struct Kind {
    set: fn(u64) -> CommandPayload,
    remove: fn(u64) -> CommandPayload,
    order: fn(&ControlService) -> Vec<u64>,
    /// The position a replayed `set` carries.
    position: fn(&CommandPayload) -> Option<u8>,
}

const SHORTCUTS: Kind = Kind {
    set: |shortcut| CommandPayload::SetShortcut {
        shortcut,
        spec: Box::new(ShortcutSpec {
            name: format!("Shortcut {shortcut}"),
            enabled: true,
            key: None,
            osc: None,
            action: WireShortcutAction::Script { script: 9 },
        }),
        position: None,
    },
    remove: |shortcut| CommandPayload::RemoveShortcut { shortcut },
    order: |control| {
        control
            .engine
            .show()
            .shortcuts()
            .iter()
            .map(|found| found.id)
            .collect()
    },
    position: |payload| match payload {
        CommandPayload::SetShortcut { position, .. } => *position,
        other => panic!("not a shortcut set: {other:?}"),
    },
};

const SCRIPTS: Kind = Kind {
    set: |script| CommandPayload::SetScript {
        script,
        spec: Box::new(ScriptSpec {
            name: format!("Script {script}"),
            source: "preview 3\n".to_owned(),
        }),
        position: None,
    },
    remove: |script| CommandPayload::RemoveScript { script },
    order: |control| {
        control
            .engine
            .show()
            .scripts()
            .iter()
            .map(|found| found.id)
            .collect()
    },
    position: |payload| match payload {
        CommandPayload::SetScript { position, .. } => *position,
        other => panic!("not a script set: {other:?}"),
    },
};

const CUE_LISTS: Kind = Kind {
    set: |list| CommandPayload::SetCueList {
        list,
        spec: Box::new(CueListSpec {
            name: format!("List {list}"),
            cues: vec![WireCue {
                id: 1,
                name: "Cut".to_owned(),
                auto_follow: false,
                actions: vec![fm_protocol::WireCueAction::Take {
                    input: Some(WireInputId::from_domain(input(3))),
                    effect: fm_protocol::TransitionFavoriteEffect::Cut,
                    duration_frames: 0,
                }],
            }],
        }),
        position: None,
    },
    remove: |list| CommandPayload::RemoveCueList { list },
    order: |control| {
        control
            .engine
            .show()
            .cue_lists()
            .iter()
            .map(|found| found.id)
            .collect()
    },
    position: |payload| match payload {
        CommandPayload::SetCueList { position, .. } => *position,
        other => panic!("not a cue list set: {other:?}"),
    },
};

fn accept(control: &mut ControlService, id: &str, payload: CommandPayload) {
    let submission = control
        .submit(&principal(Role::Admin), command(id, id, payload), 0)
        .unwrap();
    assert!(submission.is_accepted(), "{id}: {:?}", submission.output);
    control.tick(&server_identity()).unwrap();
}

/// Lists 1, 2, 3 with 2 removed.
fn removed_middle(kind: &Kind) -> ControlService {
    let mut control = service(64, 64);
    for id in [1, 2, 3] {
        accept(&mut control, &format!("set-{id}"), (kind.set)(id));
    }
    accept(&mut control, "remove-2", (kind.remove)(2));
    assert_eq!((kind.order)(&control), [1, 3]);
    control
}

fn assert_journaled_undo_keeps_the_place(kind: &Kind) {
    let mut control = removed_middle(kind);
    let message = undo("undo-remove", None);
    let staged = prepared(
        control
            .prepare_submit(&principal(Role::Admin), message.clone(), 0)
            .unwrap(),
    );
    let journaled = staged.replay_command(&message);
    assert_eq!((kind.position)(&journaled.payload), Some(1));
    assert!(staged.commit().unwrap().is_accepted());
    control.tick(&server_identity()).unwrap();
    assert_eq!((kind.order)(&control), [1, 2, 3]);

    // The journal line decodes to the same command, and replaying it on a
    // restored authority reaches the same order.
    let line = fm_protocol::encode_line(&WireMessage::Command(journaled.clone())).unwrap();
    assert!(line.contains("\tposition=1"), "{line}");
    let WireMessage::Command(decoded) = fm_protocol::decode_line(&line).unwrap() else {
        panic!("a journal line is a command");
    };
    assert_eq!(decoded, journaled);
    let mut replayed = removed_middle(kind);
    let submission = replayed
        .submit(&principal(Role::Admin), decoded, 0)
        .unwrap();
    assert!(submission.is_accepted(), "{:?}", submission.output);
    replayed.tick(&server_identity()).unwrap();
    assert_eq!((kind.order)(&replayed), [1, 2, 3]);
}

#[test]
fn an_undone_shortcut_removal_is_journaled_at_its_old_place() {
    assert_journaled_undo_keeps_the_place(&SHORTCUTS);
}

#[test]
fn an_undone_script_removal_is_journaled_at_its_old_place() {
    assert_journaled_undo_keeps_the_place(&SCRIPTS);
}

#[test]
fn an_undone_cue_list_removal_is_journaled_at_its_old_place() {
    assert_journaled_undo_keeps_the_place(&CUE_LISTS);
}
