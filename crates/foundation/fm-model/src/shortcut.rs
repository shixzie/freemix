//! User shortcuts (RC-004): project-level named actions an operator fires
//! from a key, an OSC address, or a control surface.
//!
//! A shortcut is shared by everyone who opens the project: its name, whether
//! it is enabled, its default key, its OSC name, and what it does. An
//! operator's console may override the key locally (that preference is
//! Studio's, not the project's); the action itself is always the project's.
//!
//! What a shortcut does ([`ShortcutAction`]) reuses the automation the show
//! already has rather than adding a third clock:
//!
//! - [`ShortcutAction::Actions`] runs up to [`MAX_SHORTCUT_ACTIONS`] cue
//!   actions ([`CueAction`]) together on one engine frame, in order, all or
//!   nothing, exactly as the first frame of a cue GO runs them. Waits and
//!   delays are refused here: a timed sequence is a cue.
//! - [`ShortcutAction::Cue`] operates a cue list (GO of its standby cue or of
//!   a named cue, Stop, Back, Skip, Reset), so a shortcut that fires a
//!   timed sequence fires a cue whose waits and delays the cue clock keeps.
//! - [`ShortcutAction::Script`] runs a project script, which may `wait`.
//!
//! References (inputs, cue lists, cues, scripts, stream targets) are not
//! held against the project: removing an input never fails because a
//! shortcut names it. A shortcut whose target is gone is refused when it is
//! fired, like the command it stands for.
//!
//! Keys are stored as a key name (`egui`'s [`Key::name`], ASCII letters and
//! digits) and the modifiers held with it. A key may be a chord (a second
//! stroke pressed after the first, Ctrl+K then 1) and may be scoped to one
//! console panel ([`SHORTCUT_SCOPES`]), answering only while that panel has
//! the console's focus, or to a panel's pop-out window (`<panel>-window`,
//! [`shortcut_scope_is_valid`]), answering only while that window has the
//! system's focus. A key name may instead be one of
//! [`SHORTCUT_MOUSE_BUTTONS`], a press of that mouse button with the same
//! modifiers, as either stroke of a chord (schema 80). Two enabled-or-not
//! shortcuts may not share an OSC name or hold clashing keys
//! ([`ShortcutKey::clashes`]).
//!
//! [`Key::name`]: https://docs.rs/egui/latest/egui/enum.Key.html#method.name

use core::fmt;

use crate::{CueAction, cue_action_is_valid};

/// Most shortcuts one project holds.
pub const MAX_SHORTCUTS: usize = 64;
/// Longest shortcut name, in UTF-8 bytes.
pub const MAX_SHORTCUT_NAME_BYTES: usize = 64;
/// Most actions one [`ShortcutAction::Actions`] runs.
pub const MAX_SHORTCUT_ACTIONS: usize = 16;
/// Most title text bytes one shortcut's actions carry.
pub const MAX_SHORTCUT_TEXT_BYTES: usize = 512;
/// Longest key name, in bytes.
pub const MAX_SHORTCUT_KEY_BYTES: usize = 16;
/// Longest OSC name, in bytes.
pub const MAX_SHORTCUT_OSC_BYTES: usize = 32;
/// The OSC address prefix of every user shortcut: a shortcut with OSC name
/// `intro` answers `/freemix/user/intro`, so no user address can shadow a
/// built-in one.
pub const SHORTCUT_OSC_PREFIX: &str = "/freemix/user/";

/// The console panels a shortcut key may be scoped to.
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

/// The mouse buttons a shortcut key may name in place of a key (the middle
/// button and the two side buttons, back and forward). The primary and
/// secondary buttons stay the console's.
pub const SHORTCUT_MOUSE_BUTTONS: [&str; 3] = ["MouseMiddle", "MouseBack", "MouseForward"];

/// What a window scope adds to a panel name: `mixer-window` is the Mixer
/// panel's pop-out window.
pub const SHORTCUT_WINDOW_SUFFIX: &str = "-window";

/// Whether `scope` names a console panel ([`SHORTCUT_SCOPES`]) or a panel's
/// pop-out window (the panel's name and [`SHORTCUT_WINDOW_SUFFIX`]).
#[must_use]
pub fn shortcut_scope_is_valid(scope: &str) -> bool {
    SHORTCUT_SCOPES.contains(&shortcut_scope_panel(scope))
}

/// Whether keys in scopes `first` and `second` can answer one press:
/// either is unscoped, both are the same, or one is a panel and the other
/// that panel's pop-out window (where the panel has the focus).
#[must_use]
pub fn shortcut_scopes_overlap(first: Option<&str>, second: Option<&str>) -> bool {
    let (Some(first), Some(second)) = (first, second) else {
        return true;
    };
    shortcut_scope_panel(first) == shortcut_scope_panel(second)
}

/// The panel a scope names: the panel itself, or the panel whose pop-out
/// window it is.
#[must_use]
pub fn shortcut_scope_panel(scope: &str) -> &str {
    scope.strip_suffix(SHORTCUT_WINDOW_SUFFIX).unwrap_or(scope)
}

/// A key and the modifiers held with it. `command` is Ctrl on Windows and
/// Linux and Cmd on macOS. A chord names its second stroke in `then`, a
/// plain stroke (no `then` or `scope` of its own).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ShortcutKey {
    /// The key's name: 1 to [`MAX_SHORTCUT_KEY_BYTES`] ASCII letters and
    /// digits (`A`, `F5`, `1`, `PageUp`).
    pub key: String,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
    /// A chord's second stroke.
    pub then: Option<Box<ShortcutKey>>,
    /// The panel (one of [`SHORTCUT_SCOPES`]) or pop-out window
    /// ([`shortcut_scope_is_valid`]) the key answers in; `None` answers
    /// anywhere on the console.
    pub scope: Option<String>,
}

impl ShortcutKey {
    /// A key pressed alone.
    #[must_use]
    pub fn plain(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            command: false,
            shift: false,
            alt: false,
            then: None,
            scope: None,
        }
    }

    fn stroke_is_valid(&self) -> bool {
        !self.key.is_empty()
            && self.key.len() <= MAX_SHORTCUT_KEY_BYTES
            && self.key.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }

    /// Whether the stroke is a mouse button ([`SHORTCUT_MOUSE_BUTTONS`]).
    #[must_use]
    pub fn is_mouse(&self) -> bool {
        SHORTCUT_MOUSE_BUTTONS.contains(&self.key.as_str())
    }

    /// Whether the key names are within their bounds, a second stroke is
    /// plain, and the scope is a known panel or pop-out window. Either
    /// stroke may be a mouse button (schema 80).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.stroke_is_valid()
            && self.then.as_deref().is_none_or(|then| {
                then.stroke_is_valid() && then.then.is_none() && then.scope.is_none()
            })
            && self.scope.as_deref().is_none_or(shortcut_scope_is_valid)
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

    /// Whether `self` and `other` cannot both be bound: the same first
    /// stroke where either is a single stroke or both chords end on the
    /// same second stroke, in overlapping scopes
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

impl fmt::Display for ShortcutKey {
    /// The key as the console shows it: `Ctrl+Shift+Alt+F5`, a chord
    /// `Ctrl+K, 1`, and a scoped key followed by ` in <panel>`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.command {
            formatter.write_str("Ctrl+")?;
        }
        if self.shift {
            formatter.write_str("Shift+")?;
        }
        if self.alt {
            formatter.write_str("Alt+")?;
        }
        formatter.write_str(&self.key)?;
        if let Some(then) = &self.then {
            write!(formatter, ", {then}")?;
        }
        if let Some(scope) = &self.scope {
            write!(formatter, " in {scope}")?;
        }
        Ok(())
    }
}

/// A cue list operation a shortcut performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutCueOperation {
    /// Fires the list's standby cue, or the named (nonzero) cue.
    Go { cue: Option<u64> },
    /// Drops the waiting cue's remaining actions and the armed GO.
    Stop,
    /// Moves standby to the previous cue.
    Back,
    /// Moves standby past its cue without firing it.
    Skip,
    /// Returns the list to its first cue with nothing played.
    Reset,
}

/// What a shortcut does when it is fired.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShortcutAction {
    /// Runs the actions in order on one frame, all or nothing (1 to
    /// [`MAX_SHORTCUT_ACTIONS`], no waits, Program taken at most once).
    Actions(Vec<CueAction>),
    /// Operates cue list `list` (nonzero).
    Cue {
        list: u64,
        operation: ShortcutCueOperation,
    },
    /// Runs project script `script` (nonzero).
    Script { script: u64 },
}

/// One user shortcut.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shortcut {
    /// Stable identity, nonzero and unique within the project.
    pub id: u64,
    pub name: String,
    /// A disabled shortcut keeps its bindings but is never fired.
    pub enabled: bool,
    /// The project's key for it, if any.
    pub key: Option<ShortcutKey>,
    /// Its OSC name (see [`SHORTCUT_OSC_PREFIX`]), if any.
    pub osc: Option<String>,
    pub action: ShortcutAction,
}

impl Shortcut {
    /// The full OSC address the shortcut answers, if it has an OSC name.
    #[must_use]
    pub fn osc_address(&self) -> Option<String> {
        self.osc
            .as_ref()
            .map(|name| format!("{SHORTCUT_OSC_PREFIX}{name}"))
    }
}

/// Why a shortcut or the project's shortcut set is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShortcutError {
    /// The id is zero.
    InvalidId,
    /// The name is blank, too long, or holds a control character.
    InvalidName,
    /// The key name is empty, too long, or not ASCII letters and digits.
    InvalidKey,
    /// The OSC name is empty, too long, a lone `-`, or not lowercase ASCII
    /// letters, digits, `-`, and `_`.
    InvalidOsc,
    /// The action list is empty or too long.
    InvalidActionCount,
    /// Action `action` (0-based) is out of range or a wait.
    InvalidAction {
        action: usize,
    },
    /// The actions take Program more than once.
    ConflictingTakes,
    /// The actions carry more than [`MAX_SHORTCUT_TEXT_BYTES`] of text.
    TooMuchText,
    /// A cue list, cue, or script id is zero.
    InvalidTarget,
    TooManyShortcuts,
    DuplicateId(u64),
    /// Shortcuts `first` and `second` hold clashing keys.
    DuplicateKey {
        first: u64,
        second: u64,
    },
    /// Shortcuts `first` and `second` share an OSC name.
    DuplicateOsc {
        first: u64,
        second: u64,
    },
}

impl fmt::Display for ShortcutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("shortcut id must not be zero"),
            Self::InvalidName => write!(
                formatter,
                "shortcut name must be 1 to {MAX_SHORTCUT_NAME_BYTES} bytes, not blank, \
                 without control characters"
            ),
            Self::InvalidKey => write!(
                formatter,
                "shortcut key must be 1 to {MAX_SHORTCUT_KEY_BYTES} ASCII letters and digits"
            ),
            Self::InvalidOsc => write!(
                formatter,
                "shortcut OSC name must be 1 to {MAX_SHORTCUT_OSC_BYTES} lowercase letters, \
                 digits, '-' or '_'"
            ),
            Self::InvalidActionCount => write!(
                formatter,
                "a shortcut runs 1 to {MAX_SHORTCUT_ACTIONS} actions"
            ),
            Self::InvalidAction { action } => write!(
                formatter,
                "shortcut action {} is out of range or waits; a timed sequence is a cue",
                action + 1
            ),
            Self::ConflictingTakes => formatter.write_str("a shortcut takes Program at most once"),
            Self::TooMuchText => write!(
                formatter,
                "a shortcut carries at most {MAX_SHORTCUT_TEXT_BYTES} bytes of title text"
            ),
            Self::InvalidTarget => {
                formatter.write_str("a shortcut's cue list, cue, or script id must not be zero")
            }
            Self::TooManyShortcuts => {
                write!(
                    formatter,
                    "a project holds at most {MAX_SHORTCUTS} shortcuts"
                )
            }
            Self::DuplicateId(id) => write!(formatter, "shortcut {id} is duplicated"),
            Self::DuplicateKey { first, second } => {
                write!(formatter, "shortcuts {first} and {second} share a key")
            }
            Self::DuplicateOsc { first, second } => {
                write!(
                    formatter,
                    "shortcuts {first} and {second} share an OSC name"
                )
            }
        }
    }
}

impl std::error::Error for ShortcutError {}

/// Whether `name` is a valid shortcut name.
#[must_use]
pub fn shortcut_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_SHORTCUT_NAME_BYTES
        && !name.chars().any(char::is_control)
}

/// Whether `name` is a valid OSC name: 1 to [`MAX_SHORTCUT_OSC_BYTES`]
/// lowercase ASCII letters, digits, `-`, and `_`, but not a lone `-`
/// (the protocol's "no OSC name").
#[must_use]
pub fn shortcut_osc_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name != "-"
        && name.len() <= MAX_SHORTCUT_OSC_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

/// Checks a shortcut's action on its own.
///
/// # Errors
///
/// Returns the first [`ShortcutError`].
pub fn validate_shortcut_action(action: &ShortcutAction) -> Result<(), ShortcutError> {
    match action {
        ShortcutAction::Actions(actions) => {
            if actions.is_empty() || actions.len() > MAX_SHORTCUT_ACTIONS {
                return Err(ShortcutError::InvalidActionCount);
            }
            let mut takes = 0_usize;
            let mut text = 0_usize;
            for (index, action) in actions.iter().enumerate() {
                if action.is_wait() || !cue_action_is_valid(action) {
                    return Err(ShortcutError::InvalidAction { action: index });
                }
                if action.takes_program() {
                    takes += 1;
                }
                if let CueAction::TitleText { text: value, .. } = action {
                    text += value.len();
                }
            }
            if takes > 1 {
                return Err(ShortcutError::ConflictingTakes);
            }
            if text > MAX_SHORTCUT_TEXT_BYTES {
                return Err(ShortcutError::TooMuchText);
            }
            Ok(())
        }
        ShortcutAction::Cue { list, operation } => {
            if *list == 0 || matches!(operation, ShortcutCueOperation::Go { cue: Some(0) }) {
                return Err(ShortcutError::InvalidTarget);
            }
            Ok(())
        }
        ShortcutAction::Script { script } => {
            if *script == 0 {
                return Err(ShortcutError::InvalidTarget);
            }
            Ok(())
        }
    }
}

/// Checks one shortcut's definition (not its id or its uniqueness).
///
/// # Errors
///
/// Returns the first [`ShortcutError`].
pub fn validate_shortcut_definition(shortcut: &Shortcut) -> Result<(), ShortcutError> {
    if !shortcut_name_is_valid(&shortcut.name) {
        return Err(ShortcutError::InvalidName);
    }
    if shortcut.key.as_ref().is_some_and(|key| !key.is_valid()) {
        return Err(ShortcutError::InvalidKey);
    }
    if shortcut
        .osc
        .as_deref()
        .is_some_and(|name| !shortcut_osc_is_valid(name))
    {
        return Err(ShortcutError::InvalidOsc);
    }
    validate_shortcut_action(&shortcut.action)
}

/// Checks the project's shortcuts: at most [`MAX_SHORTCUTS`], nonzero
/// unique ids, each definition valid, and no key or OSC name shared.
///
/// # Errors
///
/// Returns the first [`ShortcutError`] in list order.
pub fn validate_shortcuts(shortcuts: &[Shortcut]) -> Result<(), ShortcutError> {
    if shortcuts.len() > MAX_SHORTCUTS {
        return Err(ShortcutError::TooManyShortcuts);
    }
    for (index, shortcut) in shortcuts.iter().enumerate() {
        if shortcut.id == 0 {
            return Err(ShortcutError::InvalidId);
        }
        let earlier = &shortcuts[..index];
        if earlier.iter().any(|other| other.id == shortcut.id) {
            return Err(ShortcutError::DuplicateId(shortcut.id));
        }
        validate_shortcut_definition(shortcut)?;
        if let Some(key) = &shortcut.key
            && let Some(other) = earlier
                .iter()
                .find(|other| other.key.as_ref().is_some_and(|other| other.clashes(key)))
        {
            return Err(ShortcutError::DuplicateKey {
                first: other.id,
                second: shortcut.id,
            });
        }
        if let Some(osc) = &shortcut.osc
            && let Some(other) = earlier.iter().find(|other| other.osc.as_ref() == Some(osc))
        {
            return Err(ShortcutError::DuplicateOsc {
                first: other.id,
                second: shortcut.id,
            });
        }
    }
    Ok(())
}
