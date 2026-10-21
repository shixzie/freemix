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
    ScriptStatus::new(
        script.id,
        ScriptSpec {
            name: script.name.clone(),
            source: script.source.clone(),
        },
    )
}

/// Maps the script commands.
#[must_use]
pub fn script_engine_command(payload: &CommandPayload) -> EngineCommand {
    match payload {
        CommandPayload::SetScript {
            script,
            spec,
            position,
        } => EngineCommand::SetScript {
            script: *script,
            definition: Some(EngineScriptDefinition {
                name: spec.name.clone(),
                source: spec.source.clone(),
            }),
            position: position.map(usize::from),
        },
        CommandPayload::RemoveScript { script } => EngineCommand::SetScript {
            script: *script,
            definition: None,
            position: None,
        },
        CommandPayload::RunScript { script } => EngineCommand::RunScript { script: *script },
        CommandPayload::StopScript { run } => EngineCommand::StopScript { run: *run },
        _ => unreachable!("only script commands are delegated"),
    }
}

/// The wire form of a script edit (for undo and redo previews).
pub fn wire_edit_payload(command: &EngineCommand) -> Option<CommandPayload> {
    let EngineCommand::SetScript {
        script,
        definition,
        position,
    } = command
    else {
        return None;
    };
    Some(match definition {
        Some(definition) => CommandPayload::SetScript {
            script: *script,
            spec: Box::new(ScriptSpec {
                name: definition.name.clone(),
                source: definition.source.clone(),
            }),
            position: position.and_then(|position| u8::try_from(position).ok()),
        },
        None => CommandPayload::RemoveScript { script: *script },
    })
}

/// The run state of script `script` on `engine` (Protocol 2.62): waiting
/// or faulted when one of the show's runs is the script's, idle otherwise.
/// Since Protocol 2.69 it names the run and the line of the `wait` a
/// waiting run stopped at.
#[must_use]
pub fn run_status(engine: &Engine, script: u64) -> ScriptRunStatus {
    let runs = engine.show().script_run();
    if let Some(run) = runs.run_of(script) {
        // A waiting script is never edited, so its source parses and leads
        // to the wait; line 1 only stands in for a restore that broke that.
        let line = engine
            .show()
            .script(script)
            .and_then(|found| fm_script::parse(&found.source).ok())
            .and_then(|program| program.wait_at(&run.state))
            .map_or(1, |at| at.line.max(1));
        return ScriptRunStatus::Waiting {
            run: run.run,
            started_frame: run.started_frame,
            due_frame: run.due_frame,
            waits: run.state.waits,
            wait_frames: run.state.wait_frames,
            line,
        };
    }
    if let Some(fault) = runs.fault_of(script) {
        return ScriptRunStatus::Faulted {
            run: fault.run,
            frame: fault.frame,
            failure: fm_engine::script_failure_kind(fault.failure),
            line: fault.at.line,
            column: fault.at.column,
            detail: fault.detail.clone(),
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

/// The event of a stop of every run (Protocol 2.69): `scripts` were
/// waiting or faulted and are idle now.
pub fn script_runs_stopped(frame: u64, scripts: &[u64]) -> EventPayload {
    EventPayload::ScriptRunsStopped {
        frame,
        scripts: scripts.to_vec(),
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
    pub title_texts: &'a [(InputId, FieldId, fm_titles::FieldValue)],
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
