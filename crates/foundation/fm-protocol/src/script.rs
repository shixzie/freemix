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
//! | [`crate::EventPayload::ScriptChanged`] | `script_changed` | `script`, `name`, `source` |
//! | [`crate::EventPayload::ScriptRemoved`] | `script_removed` | `script` |
//! | [`crate::EventPayload::ScriptRan`] | `script_ran` | `script`, `frame`, `actions`, `instructions`, and the desired show state of `cue_run_changed` |
//!
//! A snapshot names how many `script_state` records follow it
//! ([`crate::SnapshotMessage::script_states`]), one per script in project
//! order, after its cue list states.

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

/// One script at a snapshot's revision. A session writes exactly
/// [`crate::SnapshotMessage::script_states`] of these after the snapshot's
/// cue list states, in project order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptStateMessage {
    pub revision: u64,
    pub script: ScriptStatus,
}

/// An accepted script run: what it did and every desired state its actions
/// may have changed (the same projection as [`crate::CueRunChange`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptRunChange {
    pub script: u64,
    /// The frame the actions reach Program on.
    pub frame: u64,
    /// Actions the run performed.
    pub actions: u32,
    /// Instructions the run used.
    pub instructions: u32,
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
