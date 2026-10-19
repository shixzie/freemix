//! Replicated project scripts (Protocol 2.58, RC-011).
//!
//! A snapshot announces one `script_state` per script; `script_changed` and
//! `script_removed` replace or drop a whole script after an edit, and
//! `script_ran` carries an accepted run's counts together with every desired
//! state its actions may have changed. A refused run publishes nothing: its
//! typed reason arrives with the command result.
//!
//! A script that waits (Protocol 2.62) runs in parts: each part arrives as
//! a `script_ran` whose `run` says whether the run waits again, and a stop,
//! a cleared fault, or a part that failed on its frame as
//! `script_run_changed`. Since Protocol 2.69 runs of several scripts wait
//! at once (each script at most one run, named by its run id), and a stop
//! of every run arrives as `script_runs_stopped`;
//! [`ProjectState::script_run_status`] reports each script's and
//! [`ProjectState::active_script_runs`] lists them all.

use fm_protocol::{ScriptRunChange, ScriptStatus};

use crate::{BusSelection, CueRunUpdate, ManualTransitionStatus, ModelError, ProjectState};

pub use fm_script::{
    FailureKind as ScriptCheckKind, ScriptFailure as ScriptCheck, Verb as ScriptVerb,
};

/// An accepted script run as a durable change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptRunUpdate {
    pub script: u64,
    /// The frame the actions reach Program on.
    pub frame: u64,
    /// Actions the run performed.
    pub actions: u32,
    /// Instructions the run used.
    pub instructions: u32,
    /// The run afterwards: waiting for its next part, or idle once it
    /// ended (Protocol 2.62).
    pub run: fm_protocol::ScriptRunStatus,
    /// The desired state after the run, in the shape of a cue run (its
    /// `list` is 0, its run state idle, and nothing fired).
    pub effects: CueRunUpdate,
}

impl ScriptRunUpdate {
    pub(crate) fn from_protocol(change: ScriptRunChange) -> Self {
        Self {
            script: change.script,
            frame: change.frame,
            actions: change.actions,
            instructions: change.instructions,
            run: change.run,
            effects: CueRunUpdate {
                list: 0,
                frame: change.frame,
                state: fm_protocol::CueRunStatus::default(),
                fired: Vec::new(),
                selection: BusSelection::new(
                    change.program.to_domain(),
                    change.preview.to_domain(),
                ),
                manual_transition: ManualTransitionStatus::from_protocol(change.manual_transition),
                fade_to_black: change.fade_to_black,
                overlays: crate::protocol_overlays(change.overlays),
                input_audio_strips: crate::protocol_input_audio_strips(change.input_audio_strips),
                streams: crate::protocol_streams(change.streams),
                recordings: change.recordings,
                playlist: change.playlist,
                titles: change.titles,
                media: change.media,
                scripts: Vec::new(),
            },
        }
    }
}

/// The last accepted run of a script, as replicated: one any client made,
/// or one a cue's script step made (Protocol 2.61).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScriptRunReport {
    pub script: u64,
    pub frame: u64,
    pub actions: u32,
    pub instructions: u32,
    /// The cue list whose script step ran it; `None` for a direct run.
    pub cue_list: Option<u64>,
}

/// Checks a whole script against the wire bounds.
pub(crate) fn validate_script(script: &ScriptStatus) -> Result<(), ModelError> {
    if script.id == 0
        || !fm_protocol::script_name_is_valid(&script.spec.name)
        || !fm_protocol::script_source_is_valid(&script.spec.source)
    {
        return Err(ModelError::InvalidScript(script.id));
    }
    Ok(())
}

/// Checks a run update against the replicated project; the caller
/// validates its switcher state like a `desired_switcher`.
pub(crate) fn validate_run(
    update: &ScriptRunUpdate,
    state: &ProjectState,
) -> Result<(), ModelError> {
    if state.script(update.script).is_none() {
        return Err(ModelError::InvalidScript(update.script));
    }
    crate::cue_lists::validate_effects(&update.effects, state)
}

/// Applies a validated run update.
pub(crate) fn apply_run(state: &mut ProjectState, update: ScriptRunUpdate) {
    record_run(
        state,
        ScriptRunReport {
            script: update.script,
            frame: update.frame,
            actions: update.actions,
            instructions: update.instructions,
            cue_list: None,
        },
    );
    set_run_status(state, update.script, update.run);
    crate::cue_lists::apply_effects(state, update.effects);
}

/// Records script `script`'s run state: each script has at most one run
/// (Protocol 2.69), so a waiting or faulted run replaces the script's own,
/// and an idle one drops it. Other scripts' runs stay.
pub(crate) fn set_run_status(
    state: &mut ProjectState,
    script: u64,
    run: fm_protocol::ScriptRunStatus,
) {
    match run {
        fm_protocol::ScriptRunStatus::Idle => {
            state.script_run_states.remove(&script);
        }
        run => {
            state.script_run_states.insert(script, run);
        }
    }
}

/// The run id a run state names, if any.
#[must_use]
pub const fn run_id(run: &fm_protocol::ScriptRunStatus) -> Option<u64> {
    match run {
        fm_protocol::ScriptRunStatus::Idle => None,
        fm_protocol::ScriptRunStatus::Waiting { run, .. }
        | fm_protocol::ScriptRunStatus::Faulted { run, .. } => Some(*run),
    }
}

/// Records `run` as its script's last run and the latest of all.
pub(crate) fn record_run(state: &mut ProjectState, run: ScriptRunReport) {
    state.script_runs.retain(|found| found.script != run.script);
    state.script_runs.push(run);
}

/// Inserts or replaces a whole script, keeping project order (new scripts
/// are appended).
pub(crate) fn apply_script(state: &mut ProjectState, script: ScriptStatus) {
    match state
        .scripts
        .iter_mut()
        .find(|candidate| candidate.id == script.id)
    {
        Some(current) => *current = script,
        None => state.scripts.push(script),
    }
}

/// The first syntax error in `source`, if any, for an editor to highlight
/// before the script is saved or run. Runtime errors, budgets, and refused
/// actions are only known once the authority runs the script.
#[must_use]
pub fn check_script_syntax(source: &str) -> Option<ScriptCheck> {
    fm_script::parse(source).err()
}

/// Whether `source` parses and holds a `wait` in any branch or loop, so
/// no cue step may run it (schema 56); a source that does not parse never
/// waits. The same test as `fm_model::script_waits`.
#[must_use]
pub fn script_waits(source: &str) -> bool {
    fm_script::parse(source).is_ok_and(|program| program.waits())
}

/// Every action verb `source` holds, in any branch: what any run of it can
/// ask for (a cue running it needs their permissions, Protocol 2.60).
/// Empty for a source that does not parse.
#[must_use]
pub fn script_verbs(source: &str) -> Vec<ScriptVerb> {
    fm_script::parse(source)
        .map(|program| program.verbs())
        .unwrap_or_default()
}
