//! Typed, structured reasons for command refusals (Protocol 2.45).
//!
//! A [`crate::Rejection`] keeps its coarse [`crate::RejectionCode`] and its
//! English message; a [`RefusalReason`] says *why* in a form clients can
//! word in their own language. A refusal without a reason (or with a reason
//! a client does not word) falls back to the message.

use core::fmt;

/// Largest [`RefusalReason::BoundsExceeded`] field token, in bytes.
pub const MAX_REFUSAL_FIELD_BYTES: usize = 64;

/// What an identifier in a refusal names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IdKind {
    Input,
    Output,
    Scene,
    /// A scene layer.
    Layer,
    /// An input category.
    Category,
    Grade,
    Lut,
    /// An animation preset.
    Preset,
    /// A title (named by its title input).
    Title,
    PlaylistEntry,
    /// A cue list (RC-017).
    CueList,
    /// A cue of a cue list (RC-017).
    Cue,
    /// A project script (RC-011).
    Script,
}

impl IdKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 13] = [
        Self::Input,
        Self::Output,
        Self::Scene,
        Self::Layer,
        Self::Category,
        Self::Grade,
        Self::Lut,
        Self::Preset,
        Self::Title,
        Self::PlaylistEntry,
        Self::CueList,
        Self::Cue,
        Self::Script,
    ];

    /// The kind's English noun.
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::Scene => "scene",
            Self::Layer => "layer",
            Self::Category => "category",
            Self::Grade => "grade",
            Self::Lut => "LUT",
            Self::Preset => "animation preset",
            Self::Title => "title",
            Self::PlaylistEntry => "playlist entry",
            Self::CueList => "cue list",
            Self::Cue => "cue",
            Self::Script => "script",
        }
    }
}

/// What keeps an input from being removed or closed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InputUse {
    Program,
    Preview,
    /// An endpoint of an automatic transition in flight.
    Transition,
    /// An endpoint of the active manual (T-bar) transition.
    ManualTransition,
    /// The source of an overlay channel (`1..=8`), on air or fading.
    Overlay {
        channel: u8,
    },
    /// A queued source of an overlay channel (`1..=8`).
    OverlayQueue {
        channel: u8,
    },
    /// The media of a Stinger slot (`1..=4`).
    Stinger {
        slot: u8,
    },
    /// A layer source of a scene.
    SceneLayer {
        scene: u128,
    },
    /// The audio source of a Scene input.
    SceneAudioSource {
        scene_input: u128,
    },
    /// The target of an alias input.
    AliasTarget {
        alias: u128,
    },
    /// The input of a playlist entry.
    PlaylistEntry {
        entry: u64,
    },
    /// Named by an action of cue `cue` in cue list `list` (RC-017).
    CueAction {
        list: u64,
        cue: u64,
    },
    /// Shown by a scene on air.
    SceneOnAir,
}

impl fmt::Display for InputUse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Program => formatter.write_str("on Program"),
            Self::Preview => formatter.write_str("on Preview"),
            Self::Transition => formatter.write_str("in a transition in flight"),
            Self::ManualTransition => formatter.write_str("in the active manual transition"),
            Self::Overlay { channel } => write!(formatter, "the source of overlay {channel}"),
            Self::OverlayQueue { channel } => write!(formatter, "queued on overlay {channel}"),
            Self::Stinger { slot } => write!(formatter, "the media of Stinger {slot}"),
            Self::SceneLayer { scene } => write!(formatter, "a layer source of scene {scene}"),
            Self::SceneAudioSource { scene_input } => {
                write!(formatter, "the audio source of Scene input {scene_input}")
            }
            Self::AliasTarget { alias } => write!(formatter, "the target of alias input {alias}"),
            Self::PlaylistEntry { entry } => write!(formatter, "played by playlist entry {entry}"),
            Self::CueAction { list, cue } => {
                write!(formatter, "named by cue {cue} of cue list {list}")
            }
            Self::SceneOnAir => formatter.write_str("shown by a scene on air"),
        }
    }
}

/// Why a script run stopped (RC-011).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScriptFailureKind {
    /// The source does not parse.
    Syntax,
    /// An expression or argument could not be evaluated.
    Runtime,
    /// The run used its whole instruction budget.
    InstructionBudget,
    /// The run performed its whole action budget.
    ActionBudget,
    /// The run defined its whole variable budget.
    VariableBudget,
    /// A string value outgrew its byte budget.
    StringBudget,
    /// The show refused one of the script's actions.
    Refused,
    /// The run passed more `wait`s than its budget (or a `wait` where
    /// none may run, in a cue's script step) (Protocol 2.62).
    WaitBudget,
    /// The run's `wait`s add up to more frames than its budget.
    WaitFramesBudget,
}

impl ScriptFailureKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 9] = [
        Self::Syntax,
        Self::Runtime,
        Self::InstructionBudget,
        Self::ActionBudget,
        Self::VariableBudget,
        Self::StringBudget,
        Self::Refused,
        Self::WaitBudget,
        Self::WaitFramesBudget,
    ];

    /// The stable wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Runtime => "runtime",
            Self::InstructionBudget => "instructions",
            Self::ActionBudget => "actions",
            Self::VariableBudget => "variables",
            Self::StringBudget => "string_bytes",
            Self::Refused => "refused",
            Self::WaitBudget => "waits",
            Self::WaitFramesBudget => "wait_frames",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }

    /// Whether a budget ran out.
    #[must_use]
    pub const fn is_budget(self) -> bool {
        matches!(
            self,
            Self::InstructionBudget
                | Self::ActionBudget
                | Self::VariableBudget
                | Self::StringBudget
                | Self::WaitBudget
                | Self::WaitFramesBudget
        )
    }
}

impl fmt::Display for ScriptFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Syntax => "syntax error",
            Self::Runtime => "runtime error",
            Self::InstructionBudget => "instruction budget exceeded",
            Self::ActionBudget => "action budget exceeded",
            Self::VariableBudget => "variable budget exceeded",
            Self::StringBudget => "string budget exceeded",
            Self::Refused => "action refused",
            Self::WaitBudget => "wait budget exceeded",
            Self::WaitFramesBudget => "wait frame budget exceeded",
        })
    }
}

/// Why a command was refused, with typed parameters.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RefusalReason {
    /// The session lacks a permission the command needs.
    PermissionDenied,
    /// The command's deadline passed before it was applied.
    DeadlineExceeded,
    /// The command expected another revision than `current`.
    RevisionConflict { current: u64 },
    /// An identifier names nothing.
    UnknownId { kind: IdKind, id: u128 },
    /// An identifier is already taken.
    DuplicateId { kind: IdKind, id: u128 },
    /// The name is already used by another item of its kind.
    DuplicateName,
    /// The name is blank or longer than `max_bytes`.
    InvalidName { max_bytes: u32 },
    /// The project-wide edit padlock is engaged.
    ProjectLocked,
    /// The input's own edit padlock is engaged.
    InputLocked { input: u128 },
    /// The input is referenced, so it cannot be removed or closed.
    InputInUse { input: u128, usage: InputUse },
    /// The display is already claimed by display output `slot`.
    DisplayInUse { slot: u8 },
    /// A bounded list or value would exceed `max`; `field` is the
    /// command field (`[a-z0-9_]`, at most [`MAX_REFUSAL_FIELD_BYTES`]).
    BoundsExceeded { field: String, max: u64 },
    /// The undo history is empty.
    NothingToUndo,
    /// The redo history is empty.
    NothingToRedo,
    /// Another edit is next in the shared history.
    HistoryConflict,
    /// The edited state changed since the history entry was recorded.
    HistoryDrift,
    /// A Stinger slot (`1..=4`) has not finished realizing; retry once it
    /// has.
    StingerRealizing { slot: u8 },
    /// A playlist take is scheduled for `frame`.
    PlaylistTakeScheduled { frame: u64 },
    /// A cue list's deferred actions are scheduled for `frame` (RC-017).
    CueScheduled { frame: u64 },
    /// GO with no cue on standby: the list is empty or ran off its end
    /// (RC-017).
    NoCueOnStandby,
    /// The edit would change or remove cue `cue` while its deferred actions
    /// wait (RC-017).
    CueRunning { cue: u64 },
    /// A script run stopped at `line`:`column` (1-based) for `failure`
    /// (RC-011); the whole run was discarded.
    ScriptFailed {
        failure: ScriptFailureKind,
        line: u32,
        column: u32,
    },
    /// Script `script` is running (waiting at a `wait`): another run of
    /// it, or an edit or removal of it, waits until it ends or is stopped
    /// (RC-011, Protocol 2.62; runs of other scripts may wait alongside it
    /// since Protocol 2.69).
    ScriptRunning { script: u64 },
    /// The command could not be made durable.
    NotDurable,
    /// The session sent too much, too fast, or too large.
    RateLimited,
    /// The daemon's media runtime cannot do this while it runs (a native
    /// daemon changes its input catalogue only offline).
    RuntimeUnsupported,
}

impl RefusalReason {
    /// The reason a rejection code alone implies, for a receipt restored
    /// without its reason (persisted receipts keep code and message only):
    /// a permission denial, a passed deadline, and a revision conflict at
    /// the receipt's `current_revision` carry nothing more.
    #[must_use]
    pub const fn implied_by(code: crate::RejectionCode, current_revision: u64) -> Option<Self> {
        match code {
            crate::RejectionCode::PermissionDenied => Some(Self::PermissionDenied),
            crate::RejectionCode::DeadlineExceeded => Some(Self::DeadlineExceeded),
            crate::RejectionCode::RevisionConflict => Some(Self::RevisionConflict {
                current: current_revision,
            }),
            _ => None,
        }
    }

    /// The stable kind token, the first token of the wire `reason`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::RevisionConflict { .. } => "revision_conflict",
            Self::UnknownId { .. } => "unknown_id",
            Self::DuplicateId { .. } => "duplicate_id",
            Self::DuplicateName => "duplicate_name",
            Self::InvalidName { .. } => "invalid_name",
            Self::ProjectLocked => "project_locked",
            Self::InputLocked { .. } => "input_locked",
            Self::InputInUse { .. } => "input_in_use",
            Self::DisplayInUse { .. } => "display_in_use",
            Self::BoundsExceeded { .. } => "bounds_exceeded",
            Self::NothingToUndo => "nothing_to_undo",
            Self::NothingToRedo => "nothing_to_redo",
            Self::HistoryConflict => "history_conflict",
            Self::HistoryDrift => "history_drift",
            Self::StingerRealizing { .. } => "stinger_realizing",
            Self::PlaylistTakeScheduled { .. } => "playlist_take_scheduled",
            Self::CueScheduled { .. } => "cue_scheduled",
            Self::NoCueOnStandby => "no_cue_on_standby",
            Self::CueRunning { .. } => "cue_running",
            Self::ScriptFailed { .. } => "script_failed",
            Self::ScriptRunning { .. } => "script_running",
            Self::NotDurable => "not_durable",
            Self::RateLimited => "rate_limited",
            Self::RuntimeUnsupported => "runtime_unsupported",
        }
    }

    /// Whether a [`Self::BoundsExceeded`] field token is well formed.
    #[must_use]
    pub fn field_is_valid(field: &str) -> bool {
        !field.is_empty()
            && field.len() <= MAX_REFUSAL_FIELD_BYTES
            && field
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    }

    /// A [`Self::BoundsExceeded`] refusal.
    #[must_use]
    pub fn bounds(field: &str, max: u64) -> Self {
        debug_assert!(Self::field_is_valid(field), "{field}");
        Self::BoundsExceeded {
            field: field.to_owned(),
            max,
        }
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied => formatter.write_str("permission denied"),
            Self::DeadlineExceeded => formatter.write_str("the command deadline passed"),
            Self::RevisionConflict { current } => {
                write!(formatter, "the project is at revision {current}")
            }
            Self::UnknownId { kind, id } => {
                write!(formatter, "{} {id} does not exist", kind.noun())
            }
            Self::DuplicateId { kind, id } => {
                write!(formatter, "{} {id} already exists", kind.noun())
            }
            Self::DuplicateName => formatter.write_str("the name is already in use"),
            Self::InvalidName { max_bytes } => write!(
                formatter,
                "the name must not be blank or exceed {max_bytes} bytes"
            ),
            Self::ProjectLocked => formatter.write_str("the project padlock is engaged"),
            Self::InputLocked { input } => write!(formatter, "input {input}'s padlock is engaged"),
            Self::InputInUse { input, usage } => write!(formatter, "input {input} is {usage}"),
            Self::DisplayInUse { slot } => {
                write!(
                    formatter,
                    "the display is already used by display output {slot}"
                )
            }
            Self::BoundsExceeded { field, max } => {
                write!(formatter, "{field} exceeds its limit of {max}")
            }
            Self::NothingToUndo => formatter.write_str("there is nothing to undo"),
            Self::NothingToRedo => formatter.write_str("there is nothing to redo"),
            Self::HistoryConflict => formatter.write_str("another edit is next in the history"),
            Self::HistoryDrift => formatter.write_str("the edited state changed since"),
            Self::StingerRealizing { slot } => {
                write!(formatter, "Stinger slot {slot} has not finished realizing")
            }
            Self::PlaylistTakeScheduled { frame } => {
                write!(formatter, "a playlist take is scheduled for frame {frame}")
            }
            Self::CueScheduled { frame } => {
                write!(formatter, "cue actions are scheduled for frame {frame}")
            }
            Self::NoCueOnStandby => formatter.write_str("no cue is on standby"),
            Self::CueRunning { cue } => write!(formatter, "cue {cue} is still running"),
            Self::ScriptFailed {
                failure,
                line,
                column,
            } => write!(
                formatter,
                "the script stopped with a {failure} at line {line}, column {column}"
            ),
            Self::ScriptRunning { script } => write!(formatter, "script {script} is running"),
            Self::NotDurable => formatter.write_str("the command could not be made durable"),
            Self::RateLimited => formatter.write_str("the session is rate limited"),
            Self::RuntimeUnsupported => {
                formatter.write_str("the running media runtime cannot do this")
            }
        }
    }
}
