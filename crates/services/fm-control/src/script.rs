//! Wire mapping of the project script commands, events, and snapshot
//! records (Protocol 2.58, RC-011), and of script waits (Protocol 2.62):
//! each script's `run` state, `script_stop`, and `script_run_changed`.

use fm_engine::{Engine, EngineCommand, EngineScriptDefinition};
use fm_model::Script;
use fm_protocol::{
    CommandPayload, EventPayload, ScriptRunChange, ScriptRunStatus, ScriptSpec, ScriptStateMessage,
    ScriptStatus, WireInputId,
};
use fm_titles::FieldId;
use fm_types::InputId;

// The wire and the model agree on every script bound.
const _: () = {
    assert!(fm_protocol::MAX_SCRIPTS == fm_model::MAX_SCRIPTS);
    assert!(fm_protocol::MAX_SCRIPT_NAME_BYTES == fm_model::MAX_SCRIPT_NAME_BYTES);
    assert!(fm_protocol::MAX_SCRIPT_SOURCE_BYTES == fm_model::MAX_SCRIPT_SOURCE_BYTES);
};

/// The wire form of a project script.
#[must_use]
pub fn wire_script(script: &Script) -> ScriptStatus {
    ScriptStatus {
        id: script.id,
        spec: ScriptSpec {
            name: script.name.clone(),
            source: script.source.clone(),
        },
    }
}

/// Maps the script commands.
#[must_use]
pub fn script_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetScript { script, spec } => EngineCommand::SetScript {
            script: *script,
            definition: Some(EngineScriptDefinition {
                name: spec.name.clone(),
                source: spec.source.clone(),
            }),
        },
        CommandPayload::RemoveScript { script } => EngineCommand::SetScript {
            script: *script,
            definition: None,
        },
        CommandPayload::RunScript { script } => EngineCommand::RunScript { script: *script },
        CommandPayload::StopScript => EngineCommand::StopScript,
        _ => unreachable!("only script commands are delegated"),
    }
}

/// The wire form of a script edit (for undo and redo previews).
pub fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetScript { script, definition } = command else {
        return None;
    };
    Some(match definition {
        Some(definition) => CommandPayload::SetScript {
            script: *script,
            spec: Box::new(ScriptSpec {
                name: definition.name.clone(),
                source: definition.source.clone(),
            }),
        },
        None => CommandPayload::RemoveScript { script: *script },
    })
}

/// The run state of script `script` on `engine` (Protocol 2.62): waiting
/// or faulted when the show's run slot names it, idle otherwise.
#[must_use]
pub fn run_status(engine: &Engine, script: u64) -> ScriptRunStatus {
    let slot = engine.show().script_run();
    if let Some(run) = slot.running.as_ref().filter(|run| run.script == script) {
        return ScriptRunStatus::Waiting {
            started_frame: run.started_frame,
            due_frame: run.due_frame,
            waits: run.state.waits,
            wait_frames: run.state.wait_frames,
        };
    }
    if let Some(fault) = slot.fault.filter(|fault| fault.script == script) {
        return ScriptRunStatus::Faulted {
            frame: fault.frame,
            failure: fm_engine::script_failure_kind(fault.failure),
            line: fault.at.line,
            column: fault.at.column,
        };
    }
    ScriptRunStatus::Idle
}

/// The event of a stop, a cleared fault, or a part that failed on its
/// frame (Protocol 2.62).
pub fn script_run_changed(engine: &Engine, script: u64, frame: u64) -> EventPayload {
    EventPayload::ScriptRunChanged {
        script,
        frame,
        run: run_status(engine, script),
    }
}

/// How many `script_state` records follow a snapshot.
pub fn script_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().scripts().len()).expect("scripts are bounded")
}

/// The `script_state` records after a snapshot, in project order.
pub fn script_states(engine: &Engine) -> Vec<ScriptStateMessage> {
    engine
        .show()
        .scripts()
        .iter()
        .map(|script| ScriptStateMessage {
            revision: engine.revision().get(),
            script: wire_script(script),
            run: run_status(engine, script.id),
        })
        .collect()
}

/// The event of an accepted script edit.
pub fn script_changed(engine: &Engine, script: u64) -> EventPayload {
    engine
        .show()
        .script(script)
        .map_or(EventPayload::ScriptRemoved { script }, |found| {
            EventPayload::ScriptChanged(Box::new(wire_script(found)))
        })
}

/// What an accepted run did, as the engine reported it.
pub struct ScriptRun<'a> {
    pub script: u64,
    pub frame: u64,
    pub actions: u32,
    pub instructions: u32,
    pub title_texts: &'a [(InputId, FieldId, String)],
    pub applied: &'a [EngineCommand],
}

/// The event of an accepted script run.
pub fn script_ran(engine: &Engine, run: &ScriptRun<'_>) -> EventPayload {
    let switcher = engine.show().desired_switcher();
    EventPayload::ScriptRan(Box::new(ScriptRunChange {
        script: run.script,
        frame: run.frame,
        actions: run.actions,
        instructions: run.instructions,
        run: run_status(engine, run.script),
        program: WireInputId::from_domain(switcher.program()),
        preview: WireInputId::from_domain(switcher.preview()),
        manual_transition: crate::protocol_manual_status(engine.desired_manual_transition()),
        fade_to_black: crate::protocol_fade_to_black_state(engine.desired_fade_to_black()),
        overlays: crate::protocol_overlays(switcher.overlays()),
        input_audio_strips: crate::protocol_input_audio_strips(engine),
        streams: crate::protocol_streams(engine),
        recordings: crate::cue::recordings(engine),
        playlist: crate::playlist::wire_run_state(engine),
        titles: crate::cue::title_texts_of(engine, run.title_texts),
        media: crate::cue::media_transports(engine, run.applied),
    }))
}
