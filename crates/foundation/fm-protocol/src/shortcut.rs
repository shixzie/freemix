//! User shortcuts (Protocol 2.70, RC-004).
//!
//! A project holds up to [`MAX_SHORTCUTS`] named shortcuts. Each has an
//! optional key (a key name and its modifiers), an optional OSC name (the
//! shortcut answers `/freemix/user/<name>`), whether it is enabled, and
//! what it does ([`WireShortcutAction`]): cue actions run together on one
//! frame, a cue list operation, or a project script run. `shortcut_set`
//! and `shortcut_remove` are undoable, padlocked project edits;
//! `shortcut_fire` fires one shortcut on the authority, which runs it as
//! the command it stands for and checks the session's authority for every
//! action it performs. A disabled or unknown shortcut is refused.
//!
//! | Message | Wire | Fields |
//! |---|---|---|
//! | [`crate::CommandPayload::SetShortcut`] | `shortcut_set` | `shortcut` (nonzero), `name`, `enabled`, `key`, `osc`, `action` |
//! | [`crate::CommandPayload::RemoveShortcut`] | `shortcut_remove` | `shortcut` |
//! | [`crate::CommandPayload::FireShortcut`] | `shortcut_fire` | `shortcut` |
//! | [`crate::EventPayload::ShortcutChanged`] | `shortcut_changed` | the whole shortcut |
//! | [`crate::EventPayload::ShortcutRemoved`] | `shortcut_removed` | `shortcut` |
//! | [`crate::EventPayload::ShortcutFired`] | `shortcut_fired` | `shortcut`, `frame`, `actions`, and the desired show state of `cue_run_changed` (without `list`, `state`, and `fired`) |
//!
//! A shortcut that operates a cue list or runs a script answers with that
//! command's own event (`cue_run_changed`, `script_ran`); `shortcut_fired`
//! reports a shortcut that ran cue actions.
//!
//! A snapshot names how many `shortcut_state` records follow it
//! ([`crate::SnapshotMessage::shortcut_states`]), one per shortcut in
//! project order, after its script states.

use crate::{
    CueMediaTransport, CueScriptRun, CueTitleText, FadeToBlackState, InputAudioStripStatus,
    ManualTransitionStatus, OverlayStatus, PlaylistRunState, RecordingStatus, StreamStatus,
    WireCueAction, WireInputId,
};

/// Most shortcuts one project holds.
pub const MAX_SHORTCUTS: usize = 64;
/// Longest shortcut name, in UTF-8 bytes.
pub const MAX_SHORTCUT_NAME_BYTES: usize = 64;
/// Most cue actions one shortcut runs.
pub const MAX_SHORTCUT_ACTIONS: usize = 16;
/// Most title text bytes one shortcut's actions carry.
pub const MAX_SHORTCUT_TEXT_BYTES: usize = 512;
/// Longest key name, in bytes.
pub const MAX_SHORTCUT_KEY_BYTES: usize = 16;
/// Longest OSC name, in bytes.
pub const MAX_SHORTCUT_OSC_BYTES: usize = 32;
/// The OSC address prefix a shortcut's OSC name follows.
pub const SHORTCUT_OSC_PREFIX: &str = "/freemix/user/";

/// A key and the modifiers held with it; `command` is Ctrl (Cmd on macOS).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WireShortcutKey {
    /// 1 to [`MAX_SHORTCUT_KEY_BYTES`] ASCII letters and digits.
    pub key: String,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
}

/// A cue list operation a shortcut performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireShortcutCueOperation {
    /// GO of the standby cue, or of the named (nonzero) cue.
    Go {
        cue: Option<u64>,
    },
    Stop,
    Back,
    Skip,
    Reset,
}

/// What a shortcut does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireShortcutAction {
    /// 1 to [`MAX_SHORTCUT_ACTIONS`] cue actions run together on one frame,
    /// without waits, taking Program at most once.
    Actions(Vec<WireCueAction>),
    /// Operates cue list `list` (nonzero).
    Cue {
        list: u64,
        operation: WireShortcutCueOperation,
    },
    /// Runs project script `script` (nonzero).
    Script { script: u64 },
}

/// A shortcut's definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutSpec {
    pub name: String,
    pub enabled: bool,
    pub key: Option<WireShortcutKey>,
    /// 1 to [`MAX_SHORTCUT_OSC_BYTES`] lowercase ASCII letters, digits,
    /// `-`, and `_`.
    pub osc: Option<String>,
    pub action: WireShortcutAction,
}

/// One project shortcut.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutStatus {
    /// Nonzero, unique within the project.
    pub id: u64,
    pub spec: ShortcutSpec,
}

/// One shortcut at a snapshot's revision. A session writes exactly
/// [`crate::SnapshotMessage::shortcut_states`] of these after the
/// snapshot's script states, in project order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutStateMessage {
    pub revision: u64,
    pub shortcut: ShortcutStatus,
}

/// A fired shortcut that ran cue actions: how many it ran and every
/// desired state they may have changed (the projection of
/// [`crate::CueRunChange`]). The actions reach Program on exactly `frame`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutFireChange {
    pub shortcut: u64,
    pub frame: u64,
    pub actions: u32,
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
    /// Every script step the actions ran, in order.
    pub scripts: Vec<CueScriptRun>,
}

/// Whether `name` is a valid shortcut name: not blank, at most
/// [`MAX_SHORTCUT_NAME_BYTES`], without control characters.
#[must_use]
pub fn shortcut_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_SHORTCUT_NAME_BYTES
        && !name.chars().any(char::is_control)
}

/// Whether `key` names a valid key.
#[must_use]
pub fn shortcut_key_is_valid(key: &WireShortcutKey) -> bool {
    !key.key.is_empty()
        && key.key.len() <= MAX_SHORTCUT_KEY_BYTES
        && key.key.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Whether `name` is a valid OSC name.
#[must_use]
pub fn shortcut_osc_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SHORTCUT_OSC_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}
