//! Wire mapping of the user shortcut commands, events, and snapshot
//! records (Protocol 2.72, RC-004).

use fm_engine::{Engine, EngineCommand};
use fm_model::{Shortcut, ShortcutAction, ShortcutCueOperation, ShortcutKey};
use fm_protocol::{
    CommandPayload, EventPayload, ShortcutFireChange, ShortcutSpec, ShortcutStateMessage,
    ShortcutStatus, WireInputId, WireShortcutAction, WireShortcutCueOperation, WireShortcutKey,
};
use fm_titles::FieldId;
use fm_types::InputId;

// The wire and the model agree on every shortcut bound.
const _: () = {
    assert!(fm_protocol::MAX_SHORTCUTS == fm_model::MAX_SHORTCUTS);
    assert!(fm_protocol::MAX_SHORTCUT_NAME_BYTES == fm_model::MAX_SHORTCUT_NAME_BYTES);
    assert!(fm_protocol::MAX_SHORTCUT_ACTIONS == fm_model::MAX_SHORTCUT_ACTIONS);
    assert!(fm_protocol::MAX_SHORTCUT_TEXT_BYTES == fm_model::MAX_SHORTCUT_TEXT_BYTES);
    assert!(fm_protocol::MAX_SHORTCUT_KEY_BYTES == fm_model::MAX_SHORTCUT_KEY_BYTES);
    assert!(fm_protocol::MAX_SHORTCUT_OSC_BYTES == fm_model::MAX_SHORTCUT_OSC_BYTES);
    assert!(same_text(
        fm_protocol::SHORTCUT_OSC_PREFIX,
        fm_model::SHORTCUT_OSC_PREFIX
    ));
};

/// `a == b`, at compile time.
const fn same_text(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The wire form of a shortcut's key.
#[must_use]
pub fn wire_key(key: &ShortcutKey) -> WireShortcutKey {
    WireShortcutKey {
        key: key.key.clone(),
        command: key.command,
        shift: key.shift,
        alt: key.alt,
        then: key.then.as_deref().map(|then| Box::new(wire_key(then))),
        scope: key.scope.clone(),
    }
}

/// The model form of a wire key.
#[must_use]
pub fn domain_key(key: &WireShortcutKey) -> ShortcutKey {
    ShortcutKey {
        key: key.key.clone(),
        command: key.command,
        shift: key.shift,
        alt: key.alt,
        then: key.then.as_deref().map(|then| Box::new(domain_key(then))),
        scope: key.scope.clone(),
    }
}

/// The wire form of a shortcut's action.
#[must_use]
pub fn wire_action(action: &ShortcutAction) -> WireShortcutAction {
    match action {
        ShortcutAction::Actions(actions) => {
            WireShortcutAction::Actions(actions.iter().map(crate::cue::wire_action).collect())
        }
        ShortcutAction::Cue { list, operation } => WireShortcutAction::Cue {
            list: *list,
            operation: match operation {
                ShortcutCueOperation::Go { cue } => WireShortcutCueOperation::Go { cue: *cue },
                ShortcutCueOperation::Stop => WireShortcutCueOperation::Stop,
                ShortcutCueOperation::Back => WireShortcutCueOperation::Back,
                ShortcutCueOperation::Skip => WireShortcutCueOperation::Skip,
                ShortcutCueOperation::Reset => WireShortcutCueOperation::Reset,
            },
        },
        ShortcutAction::Script { script } => WireShortcutAction::Script { script: *script },
    }
}

/// The model form of a wire action.
#[must_use]
pub fn domain_action(action: &WireShortcutAction) -> ShortcutAction {
    match action {
        WireShortcutAction::Actions(actions) => {
            ShortcutAction::Actions(actions.iter().map(crate::cue::domain_action).collect())
        }
        WireShortcutAction::Cue { list, operation } => ShortcutAction::Cue {
            list: *list,
            operation: match operation {
                WireShortcutCueOperation::Go { cue } => ShortcutCueOperation::Go { cue: *cue },
                WireShortcutCueOperation::Stop => ShortcutCueOperation::Stop,
                WireShortcutCueOperation::Back => ShortcutCueOperation::Back,
                WireShortcutCueOperation::Skip => ShortcutCueOperation::Skip,
                WireShortcutCueOperation::Reset => ShortcutCueOperation::Reset,
            },
        },
        WireShortcutAction::Script { script } => ShortcutAction::Script { script: *script },
    }
}

/// The wire form of a project shortcut.
#[must_use]
pub fn wire_shortcut(shortcut: &Shortcut) -> ShortcutStatus {
    ShortcutStatus {
        id: shortcut.id,
        spec: ShortcutSpec {
            name: shortcut.name.clone(),
            enabled: shortcut.enabled,
            key: shortcut.key.as_ref().map(wire_key),
            osc: shortcut.osc.clone(),
            action: wire_action(&shortcut.action),
        },
    }
}

/// The model form of a wire shortcut `id` with `spec`.
#[must_use]
pub fn domain_shortcut(id: u64, spec: &ShortcutSpec) -> Shortcut {
    Shortcut {
        id,
        name: spec.name.clone(),
        enabled: spec.enabled,
        key: spec.key.as_ref().map(domain_key),
        osc: spec.osc.clone(),
        action: domain_action(&spec.action),
    }
}

/// Maps the shortcut commands.
#[must_use]
pub fn shortcut_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetShortcut {
            shortcut,
            spec,
            position,
        } => EngineCommand::SetShortcut {
            shortcut: *shortcut,
            definition: Some(Box::new(domain_shortcut(*shortcut, spec))),
            position: position.map(usize::from),
        },
        CommandPayload::RemoveShortcut { shortcut } => EngineCommand::SetShortcut {
            shortcut: *shortcut,
            definition: None,
            position: None,
        },
        CommandPayload::FireShortcut { shortcut } => EngineCommand::FireShortcut {
            shortcut: *shortcut,
        },
        _ => unreachable!("only shortcut commands are delegated"),
    }
}

/// The wire form of a shortcut edit (for undo and redo previews).
pub fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetShortcut {
        shortcut,
        definition,
        position,
    } = command
    else {
        return None;
    };
    Some(match definition {
        Some(definition) => CommandPayload::SetShortcut {
            shortcut: *shortcut,
            spec: Box::new(wire_shortcut(definition).spec),
            position: position.and_then(|position| u8::try_from(position).ok()),
        },
        None => CommandPayload::RemoveShortcut {
            shortcut: *shortcut,
        },
    })
}

/// How many `shortcut_state` records follow a snapshot.
pub fn shortcut_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().shortcuts().len()).expect("shortcuts are bounded")
}

/// The `shortcut_state` records after a snapshot, in project order.
pub fn shortcut_states(engine: &Engine) -> Vec<ShortcutStateMessage> {
    engine
        .show()
        .shortcuts()
        .iter()
        .map(|shortcut| ShortcutStateMessage {
            revision: engine.revision().get(),
            shortcut: wire_shortcut(shortcut),
        })
        .collect()
}

/// The event of an accepted shortcut edit.
pub fn shortcut_changed(engine: &Engine, shortcut: u64) -> EventPayload {
    engine
        .show()
        .shortcut(shortcut)
        .map_or(EventPayload::ShortcutRemoved { shortcut }, |found| {
            EventPayload::ShortcutChanged(Box::new(wire_shortcut(found)))
        })
}

/// What an accepted fire of cue actions did, as the engine reported it.
pub struct ShortcutFire<'a> {
    pub shortcut: u64,
    pub frame: u64,
    pub actions: u32,
    pub title_texts: &'a [(InputId, FieldId, fm_titles::FieldValue)],
    pub applied: &'a [EngineCommand],
    pub scripts: &'a [fm_engine::CueScriptRun],
}

/// The event of an accepted fire of cue actions.
pub fn shortcut_fired(engine: &Engine, fire: &ShortcutFire<'_>) -> EventPayload {
    let switcher = engine.show().desired_switcher();
    EventPayload::ShortcutFired(Box::new(ShortcutFireChange {
        shortcut: fire.shortcut,
        frame: fire.frame,
        actions: fire.actions,
        program: WireInputId::from_domain(switcher.program()),
        preview: WireInputId::from_domain(switcher.preview()),
        manual_transition: crate::protocol_manual_status(engine.desired_manual_transition()),
        fade_to_black: crate::protocol_fade_to_black_state(engine.desired_fade_to_black()),
        overlays: crate::protocol_overlays(switcher.overlays()),
        input_audio_strips: crate::protocol_input_audio_strips(engine),
        streams: crate::protocol_streams(engine),
        recordings: crate::cue::recordings(engine),
        playlist: crate::playlist::wire_run_state(engine),
        titles: crate::cue::title_texts_of(engine, fire.title_texts),
        media: crate::cue::media_transports(engine, fire.applied),
        scripts: fire
            .scripts
            .iter()
            .map(|ran| fm_protocol::CueScriptRun {
                script: ran.script,
                actions: ran.actions,
                instructions: ran.instructions,
            })
            .collect(),
    }))
}
