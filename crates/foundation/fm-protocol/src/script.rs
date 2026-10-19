//! Project scripts (Protocol 2.58, RC-011).
//!
//! A project holds up to [`MAX_SCRIPTS`] named `FreeMix` Script sources.
//! `script_set` and `script_remove` are undoable, padlocked project edits;
//! `script_run` runs one script on the authority, which performs its actions
//! as ordinary show commands on the command's target frame, all or nothing.
//! A run that stops early (a syntax or runtime error, an exhausted budget,
//! or a refused action) is refused with the typed
//! [`fm_command::RefusalReason::ScriptFailed`] reason naming the line and
//! column; nothing it did is kept.
//!
//! | Message | Wire | Fields |
//! |---|---|---|
//! | [`crate::CommandPayload::SetScript`] | `script_set` | `script` (nonzero), `name`, `source` (lowercase hex of its UTF-8 bytes) |
//! | [`crate::CommandPayload::RemoveScript`] | `script_remove` | `script` |
//! | [`crate::CommandPayload::RunScript`] | `script_run` | `script` |
//! | [`crate::CommandPayload::StopScript`] | `script_stop` | optional `run` (Protocol 2.62; `run` since 2.69) |
//! | [`crate::EventPayload::ScriptChanged`] | `script_changed` | `script`, `name`, `source` |
//! | [`crate::EventPayload::ScriptRemoved`] | `script_removed` | `script` |
//! | [`crate::EventPayload::ScriptRan`] | `script_ran` | `script`, `frame`, `actions`, `instructions`, `run` (Protocol 2.62), and the desired show state of `cue_run_changed` |
//! | [`crate::EventPayload::ScriptRunChanged`] | `script_run_changed` | `script`, `frame`, `run` (Protocol 2.62) |
//! | [`crate::EventPayload::ScriptRunsStopped`] | `script_runs_stopped` | `frame`, `scripts` (Protocol 2.69) |
//!
//! A snapshot names how many `script_state` records follow it
//! ([`crate::SnapshotMessage::script_states`]), one per script in project
//! order, after its cue list states; each carries the script's `run`
//! (Protocol 2.62).
//!
//! # Waits (Protocol 2.62)
//!
//! A script's `wait FRAMES` spans frames: the run's parts after a wait run
//! on their due frame, on the show as it is then. Since Protocol 2.69 the
//! authority keeps up to [`MAX_SCRIPT_RUNS`] runs waiting at once, each of
//! a different script, saved with the project; its cue clock resumes them
//! with `cue_advance` naming list `0` (the earliest due run first, the
//! lowest run id on a tie). `run` reports a script's run state
//! ([`ScriptRunStatus`]): `idle`, `waiting:<run>:<started frame>:<due
//! frame>:<waits passed>:<frames waited>:<wait line>`, or
//! `faulted:<run>:<frame>:<failure>:<line>:<column>:<reason>:<message>`
//! when a part failed on its frame
//! (nothing of that frame applied; `failure` a
//! [`fm_command::ScriptFailureKind`] token). Since Protocol 2.63 the fault
//! says what the run was refused with ([`fm_command::FaultDetail`]):
//! `reason` is the lowercase hex of a refused action's `reason` token (`-`
//! when the refusal had none, or the run failed without a refused action)
//! and `message` the lowercase hex of the English refusal text, one line of
//! 1 to [`fm_command::MAX_FAULT_MESSAGE_BYTES`] bytes without control
//! characters or surrounding whitespace.
//! `script_ran` reports each part that ran (counts cover the whole run so
//! far) and `script_run_changed` a stop, a cleared fault, or a fault.
//! `script_stop run=<run>` ends that run or clears its fault, and
//! `script_stop` alone ends every run and clears every fault
//! (`script_runs_stopped` names the scripts it idled). A `script_run` of a
//! script already waiting, or an edit or removal of it, is refused with
//! `script_running:<script>` until then; a run that would wait while
//! [`MAX_SCRIPT_RUNS`] already do is refused with
//! `bounds_exceeded:script_runs:8`.

use crate::{
    CueMediaTransport, CueTitleText, FadeToBlackState, InputAudioStripStatus,
    ManualTransitionStatus, OverlayStatus, PlaylistRunState, RecordingStatus, StreamStatus,
    WireInputId,
};

/// Most scripts one project holds.
pub const MAX_SCRIPTS: usize = 16;
/// Longest script name, in UTF-8 bytes.
pub const MAX_SCRIPT_NAME_BYTES: usize = 64;
/// Longest script source, in UTF-8 bytes.
pub const MAX_SCRIPT_SOURCE_BYTES: usize = 4_096;
/// Most script runs waiting at once (Protocol 2.69).
pub const MAX_SCRIPT_RUNS: usize = 8;

/// A script's name and source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptSpec {
    pub name: String,
    pub source: String,
}

/// One project script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptStatus {
    /// Nonzero, unique within the project.
    pub id: u64,
    pub spec: ScriptSpec,
}

/// A script's run (Protocol 2.62). Since Protocol 2.69 several scripts
/// run at once, at most [`MAX_SCRIPT_RUNS`] waiting, and each run names
/// its id: nonzero, issued in order by the authority, never reused.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ScriptRunStatus {
    /// Not running, and the last run did not fault.
    #[default]
    Idle,
    /// Run `run` waits at the `wait` on `line` (1-based, Protocol 2.69):
    /// started on `started_frame`, resumes on `due_frame`, having passed
    /// `waits` waits of `wait_frames` frames in all.
    Waiting {
        run: u64,
        started_frame: u64,
        due_frame: u64,
        waits: u32,
        wait_frames: u32,
        line: u32,
    },
    /// The part of run `run` due on `frame` failed at `line`:`column`
    /// (1-based): nothing of that frame applied and the run ended.
    /// `detail` says what the run was refused with (Protocol 2.63): a
    /// refused action's typed reason and message, else the failure's own
    /// message.
    Faulted {
        run: u64,
        frame: u64,
        failure: fm_command::ScriptFailureKind,
        line: u32,
        column: u32,
        detail: fm_command::FaultDetail,
    },
}

/// One script at a snapshot's revision. A session writes exactly
/// [`crate::SnapshotMessage::script_states`] of these after the snapshot's
/// cue list states, in project order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptStateMessage {
    pub revision: u64,
    pub script: ScriptStatus,
    /// The script's run (Protocol 2.62).
    pub run: ScriptRunStatus,
}

/// An accepted script run, or one part of a run that waited: what it did
/// (counts cover the whole run so far) and every desired state its
/// actions may have changed (the same projection as
/// [`crate::CueRunChange`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptRunChange {
    pub script: u64,
    /// The frame the actions reach Program on.
    pub frame: u64,
    /// Actions the run performed.
    pub actions: u32,
    /// Instructions the run used.
    pub instructions: u32,
    /// The run afterwards: waiting for its next part, or idle once it
    /// ended (Protocol 2.62).
    pub run: ScriptRunStatus,
    pub program: WireInputId,
    pub preview: WireInputId,
    pub manual_transition: ManualTransitionStatus,
    pub fade_to_black: FadeToBlackState,
    pub overlays: Vec<OverlayStatus>,
    pub input_audio_strips: Vec<InputAudioStripStatus>,
    pub streams: Vec<StreamStatus>,
    pub recordings: Vec<RecordingStatus>,
    pub playlist: PlaylistRunState,
    pub titles: Vec<CueTitleText>,
    pub media: Vec<CueMediaTransport>,
}

/// Whether `name` is a valid script name: not blank, at most
/// [`MAX_SCRIPT_NAME_BYTES`], without control characters.
#[must_use]
pub fn script_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_SCRIPT_NAME_BYTES
        && !name.chars().any(char::is_control)
}

/// Whether `source` is a valid script source: at most
/// [`MAX_SCRIPT_SOURCE_BYTES`], its only control characters newlines,
/// tabs, and carriage returns.
#[must_use]
pub fn script_source_is_valid(source: &str) -> bool {
    source.len() <= MAX_SCRIPT_SOURCE_BYTES
        && !source
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t' | '\r'))
}
