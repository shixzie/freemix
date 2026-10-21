//! User shortcuts (Protocol 2.72, RC-004).
//!
//! A project holds up to [`MAX_SHORTCUTS`] named shortcuts. Each has an
//! optional key (a key name and its modifiers; since 2.83 optionally a
//! chord's second stroke and the console panel the key is scoped to; since
//! 2.93 a mouse button in place of a key and a panel's pop-out window as
//! its scope; since 2.106 one of Studio's tool windows as its scope), an
//! optional OSC name (the
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
//! | [`crate::CommandPayload::SetShortcut`] | `shortcut_set` | `shortcut` (nonzero), `name`, `enabled`, `key`, `osc`, `action`, optional `position` (2.79, below `MAX_SHORTCUTS`) |
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
//! Since Protocol 2.104 (GX-006) a Web session's `shortcut_state` and
//! `shortcut_changed` carry a title action setting a field hidden from it
//! as [`WireCueAction::TitleHidden`]; see [`crate::HIDDEN_TITLE_VALUE`].
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

/// The console panels a shortcut key may be scoped to (Protocol 2.83):
/// a scoped key answers only while that panel has the console's focus.
pub const SHORTCUT_SCOPES: [&str; 12] = [
    "preview",
    "transitions",
    "program",
    "inputs",
    "mixer",
    "outputs",
    "overlays",
    "stingers",
    "titles",
    "playlist",
    "media",
    "cues",
];

/// The mouse buttons a shortcut key may name in place of a key (Protocol
/// 2.93): the middle button and the two side buttons, back and forward.
/// Since Protocol 2.99 a mouse button may be either stroke of a chord.
pub const SHORTCUT_MOUSE_BUTTONS: [&str; 3] = ["MouseMiddle", "MouseBack", "MouseForward"];

/// What a window scope adds to a panel name (Protocol 2.93):
/// `mixer-window` is the Mixer panel's pop-out window, and a key scoped to
/// it answers only while that window has the system's focus.
pub const SHORTCUT_WINDOW_SUFFIX: &str = "-window";

/// The tool windows a shortcut key may be scoped to (Protocol 2.106), each
/// followed by [`SHORTCUT_WINDOW_SUFFIX`]: `color-window` (the Colour
/// window), `animation-window` (the keyframe Animation editor), and
/// `designer-window` (the scene Designer). A key scoped to one answers only
/// while that window has the focus, over the console or popped out. A tool
/// window has no panel scope of its own.
pub const SHORTCUT_TOOL_WINDOWS: [&str; 3] = ["color", "animation", "designer"];

/// The panel or tool window a scope names: the panel itself, or the panel
/// or tool ([`SHORTCUT_TOOL_WINDOWS`]) whose window it is.
#[must_use]
pub fn shortcut_scope_panel(scope: &str) -> &str {
    scope.strip_suffix(SHORTCUT_WINDOW_SUFFIX).unwrap_or(scope)
}

/// Whether `scope` names a console panel ([`SHORTCUT_SCOPES`]), a panel's
/// pop-out window, or a tool window ([`SHORTCUT_TOOL_WINDOWS`]).
#[must_use]
pub fn shortcut_scope_is_valid(scope: &str) -> bool {
    match scope.strip_suffix(SHORTCUT_WINDOW_SUFFIX) {
        Some(window) => {
            SHORTCUT_SCOPES.contains(&window) || SHORTCUT_TOOL_WINDOWS.contains(&window)
        }
        None => SHORTCUT_SCOPES.contains(&scope),
    }
}

/// Whether keys in scopes `first` and `second` can answer one press:
/// either is unscoped, or both name the same panel (the panel itself or
/// its pop-out window, where that panel has the focus) or the same tool
/// window.
#[must_use]
pub fn shortcut_scopes_overlap(first: Option<&str>, second: Option<&str>) -> bool {
    match (first, second) {
        (Some(first), Some(second)) => shortcut_scope_panel(first) == shortcut_scope_panel(second),
        _ => true,
    }
}

/// A key and the modifiers held with it; `command` is Ctrl (Cmd on macOS).
///
/// Since Protocol 2.83 a key may be a chord, `then` naming the second
/// stroke pressed after it (Ctrl+K, then 1), and may be scoped to one
/// console panel. The second stroke is a plain stroke: no `then` or
/// `scope` of its own. Since 2.93 `key` may name a mouse button
/// ([`SHORTCUT_MOUSE_BUTTONS`]) and the scope may be a panel's pop-out
/// window ([`shortcut_scope_is_valid`]); since 2.99 a mouse button may be
/// a chord's first or second stroke; since 2.106 the scope may be a tool
/// window ([`SHORTCUT_TOOL_WINDOWS`]).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WireShortcutKey {
    /// 1 to [`MAX_SHORTCUT_KEY_BYTES`] ASCII letters and digits: a key's
    /// name or one of [`SHORTCUT_MOUSE_BUTTONS`].
    pub key: String,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
    /// A chord's second stroke.
    pub then: Option<Box<WireShortcutKey>>,
    /// The panel (one of [`SHORTCUT_SCOPES`]), pop-out window, or tool
    /// window ([`SHORTCUT_TOOL_WINDOWS`]) the key answers in; `None` answers anywhere on the console.
    pub scope: Option<String>,
}

impl WireShortcutKey {
    /// A key pressed alone with these modifiers, anywhere on the console.
    #[must_use]
    pub fn stroke(key: impl Into<String>, command: bool, shift: bool, alt: bool) -> Self {
        Self {
            key: key.into(),
            command,
            shift,
            alt,
            then: None,
            scope: None,
        }
    }

    /// Whether the stroke is a mouse button ([`SHORTCUT_MOUSE_BUTTONS`]).
    #[must_use]
    pub fn is_mouse(&self) -> bool {
        SHORTCUT_MOUSE_BUTTONS.contains(&self.key.as_str())
    }

    /// Whether the first strokes of `self` and `other` are the same key
    /// with the same modifiers.
    #[must_use]
    pub fn same_stroke(&self, other: &Self) -> bool {
        self.key == other.key
            && self.command == other.command
            && self.shift == other.shift
            && self.alt == other.alt
    }

    /// Whether `self` and `other` cannot both be bound in one project: the
    /// same first stroke where either is a single stroke or both chords
    /// end on the same second stroke, in overlapping scopes
    /// ([`shortcut_scopes_overlap`]).
    #[must_use]
    pub fn clashes(&self, other: &Self) -> bool {
        let strokes = self.same_stroke(other)
            && match (&self.then, &other.then) {
                (Some(mine), Some(theirs)) => mine.same_stroke(theirs),
                _ => true,
            };
        strokes && shortcut_scopes_overlap(self.scope.as_deref(), other.scope.as_deref())
    }
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
    /// `-`, and `_`, but not a lone `-`.
    pub osc: Option<String>,
    pub action: WireShortcutAction,
}

impl ShortcutSpec {
    /// Whether the shortcut's cue actions carry a
    /// [`WireCueAction::TitleHidden`] marker (Protocol 2.104).
    #[must_use]
    pub fn has_hidden_title_value(&self) -> bool {
        matches!(&self.action, WireShortcutAction::Actions(actions)
            if actions
                .iter()
                .any(|action| matches!(action, WireCueAction::TitleHidden { .. })))
    }
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

/// Whether `key` names a valid key: names within their bounds, a plain
/// second stroke, and a known panel, pop-out window, or tool window as its
/// scope.
#[must_use]
pub fn shortcut_key_is_valid(key: &WireShortcutKey) -> bool {
    let stroke = |key: &WireShortcutKey| {
        !key.key.is_empty()
            && key.key.len() <= MAX_SHORTCUT_KEY_BYTES
            && key.key.bytes().all(|byte| byte.is_ascii_alphanumeric())
    };
    stroke(key)
        && key
            .then
            .as_deref()
            .is_none_or(|then| stroke(then) && then.then.is_none() && then.scope.is_none())
        && key.scope.as_deref().is_none_or(shortcut_scope_is_valid)
}

/// Whether `name` is a valid OSC name. A lone `-` is not: the `osc` field
/// writes it for "no OSC name".
#[must_use]
pub fn shortcut_osc_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name != "-"
        && name.len() <= MAX_SHORTCUT_OSC_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}
